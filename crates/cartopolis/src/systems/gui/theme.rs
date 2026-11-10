//! The Cartopolis look: one palette, one `egui::Style`, one place to change them.
//!
//! **Deep teal.** A teal-grey ramp — `#0F1416` ink, `#141A1C` surface,
//! `#1F2829` raised, `#2A3536` hover — carries the chrome, and the live things
//! are a clear **teal**: [`Palette::accent`] `#5CC6C0`, with [`Palette::ink`] on it, and its dark
//! half [`Palette::accent_muted`] `#263F42` for a selected row or a pressed button. The
//! values are read off Android's keyboard in its teal dark theme (2026-10-10),
//! which is the look asked for: the surface, the key and the shift-key teal are
//! that keyboard's own. [`Palette::warn`] and [`Palette::error`] are the only other hues, and
//! [`BRAND`] — the logo's orange — stays with the mark, the wordmark and the
//! badges, where against teal it is the logo's original pairing.
//!
//! **How it got here, so nobody walks the same circle.** The ramp was a slate
//! blue with the full brand orange as accent; then (2026-09-10) it went
//! monochrome — a blue-teal grey with a *pale grey* accent — on the argument
//! that a saturated accent competes with a colourful map; then (2026-10-04)
//! the grey was turned toward orange, the panels tinted brown, and finally a
//! neutral charcoal with an orange "ember" accent. The pale grey accent was
//! rejected because every selected tab, ticked box and engaged button read as
//! one more shade of grey — the UI looked dull — and orange mixed *into* the
//! surfaces read as brown. The charcoal and ember was replaced at the user's
//! request for a teal look shared with the web UI (`web_theme`). What survives
//! all of them is the usual rule for a dark UI: **quiet surfaces, one saturated
//! accent, used sparingly**, appearing only where something is selected,
//! engaged, pressed or focused — which keeps it meaning "this is the live
//! thing" over a map. The accent being a *saturated* teal rather than the pale
//! grey of 2026-09 is the difference that matters.
//!
//! Three consequences worth knowing before editing a value:
//!
//!   - **The state ladder is monotonic in lightness** — rest ([`Palette::surface_raised`])
//!     → hover ([`Palette::surface_hover`] plus a [`Palette::accent_word`] rim) → pressed
//!     ([`Palette::accent_muted`], a deep teal, plus the same rim) → engaged
//!     (an [`Palette::accent`] plate carrying [`Palette::on_accent`]). Each step is up, so state still
//!     reads for someone who cannot see the hue.
//!   - **Text on the accent is [`Palette::on_accent`], white** (since 2026-11-10,
//!     on a deep teal; it was dark ink on a bright one). A pressed button keeps
//!     its fill [`Palette::accent_muted`] (see [`widgets`]), so its label stays
//!     [`Palette::cream`].
//!   - **Two constants deliberately share a value.** [`Palette::accent_text`] and
//!     [`Palette::accent_bright`] are both pure white: one is type, one is a stroke, they
//!     answer to different tests. Keeping both names is what lets one move
//!     without the other.
//!
//! **Every surface is solid** (since 2026-11-08, "a bad implementation of
//! glass"): the midas hub's layering, one darkness. [`Palette::panel`], [`Palette::glass`] and
//! [`Palette::overlay`] are all one dark teal, with [`Palette::card`] blocks inside, close to it, and a soft
//! shadow, with the hub's hairline ([`Palette::panel_rim`]). Nothing shows through.
//!
//! Two things this replaces, both of which the UI used to get by default:
//!
//!   - **egui's stock dark theme**, which is a neutral grey (`gray(27)` panels)
//!     with a blue selection. Nothing about it said Cartopolis, and next to the
//!     map — which is bright — flat grey read as a hole rather than a surface.
//!   - **egui's stock spacing**, which is tuned for a debug inspector: 3 pt
//!     between rows and 1 pt of vertical button padding. In a panel of place
//!     rows and layer toggles that is a wall of text; the values here roughly
//!     double the vertical rhythm and give buttons a real hit area.
//!
//! Contrast was checked against the surface each colour actually lands on:
//! [`Palette::text`] is 14.3:1 on [`Palette::surface`], [`Palette::text_dim`] 9.4:1, [`Palette::accent_text`] 17.6:1.
//! Plain [`Palette::accent`] is 3.9:1, enough for a fill, a rule or a ring (3:1) on any
//! surface in the ramp. Anything sitting *on* an accent fill is drawn in
//! [`Palette::on_accent`], white, at 4.5:1.
//! (The numbers in this note are the dark theme's.)
//!
//! **Two palettes, since 2026-11-10.** Everything above describes [`DARK`],
//! which is the look as it was, value for value. [`LIGHT`] is the midas hub's
//! light tokens put in the same roles; the chrome reads the one in force
//! through [`p`], and `systems::ui_theme` picks it from the app's light
//! setting (Light, Dark, or Auto by the sun). The map's own labels, the splash,
//! the call and camera screens and the script editor's code box stay dark in
//! both ([`dark_here`], [`use_dark_visuals`]).
//!
//! [`install`] is called once, from `EguiManager::default`, before the first
//! pass — early enough that the brand font family is bound on frame one, which
//! is what lets the splash paint its wordmark immediately.
//!
//! **Four scales, and nothing outside them.** Colour was the only axis this file
//! originally owned, and the drift went where there was no vocabulary to drift
//! from: the app shipped 17 text sizes and 8 corner radii against one exported
//! radius, because a panel that needed "a bit smaller than body" had nothing to
//! name. So the type scale ([`SIZE_SUB`] and friends), the corner radii
//! ([`RADIUS`], [`RADIUS_SM`], [`RADIUS_LG`], [`RADIUS_PILL`]) and the touch
//! sizes ([`MIN_TOUCH_TARGET_DP`], [`row_height`]) are exported beside the
//! palette. Pick by **meaning**, never by the number — a call site that reads
//! `SIZE_SUB` tells the next person what the text *is*, and `12.5` tells them
//! only what someone once liked.

use egui::{
    style::{Selection, WidgetVisuals, Widgets},
    Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
};

use std::sync::atomic::{AtomicBool, Ordering};

use crate::systems::icons;

// ─── Palette ──────────────────────────────────────────────────────────────────

/// The chrome's tones, one field per role. There are two of them, [`DARK`] and
/// [`LIGHT`], and [`p`] answers the one in force; every chrome colour in the
/// client is `theme::Palette::field`, read when it is painted, so flipping the
/// theme ([`set_light`]) repaints the next frame without anything holding a
/// copy.
///
/// **Map a role, not a value.** The light palette is the midas hub's light
/// tokens (midas `web/src/theme.css`, the first value of each `light-dark()`,
/// which are [`web_theme`](super::web_theme)'s `L_*`), each put in the slot
/// whose *job* it does: [`Palette::panel`] is the page, [`Palette::card`] the
/// white card on it, [`Palette::cream`] the strongest text — near-black in the
/// light theme — and [`Palette::line`] the hub's strong border, since it is
/// what identifies a control.
///
/// Brand and data colours are not here: [`BRAND`], [`REAL_LOCATION`],
/// [`PUBLIC`], the council, the code colours, the map labels
/// ([`LABEL_CHIP`] and friends), the call buttons and the scenario inks stay
/// `const`, the same in both themes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Deeper than any of the surface tones: text-edit wells, code blocks, a
    /// segmented control's track. In the light theme it is the hub's sunken
    /// tone. **Not the ink on an accent plate** — that is [`Palette::on_accent`],
    /// which shared this value until the light theme split them (2026-11-10).
    pub ink: Color32,
    /// The ink on an [`Palette::accent`] plate (an engaged disc, a ticked box,
    /// the primary button's label): white in both themes, on a deep teal
    /// (`#2B837E`, 4.5:1). Until 2026-11-10 the plate was the bright
    /// `#5CC6C0` with `#0F1416` on it, and the person found "a teal button
    /// with black text" not nice.
    pub on_accent: Color32,
    /// The ink on a status-coloured plate ([`Palette::error`], [`Palette::warn`]
    /// as a fill — an armed confirm, a raised hand): [`Palette::ink`] in the
    /// dark theme, where the status tones are light; white in the light theme,
    /// where they are dark.
    pub on_status: Color32,
    /// The surface ramp's bottom above [`Palette::ink`]: control plates (a
    /// button at rest) and the widget table's noninteractive fill. A card in a
    /// panel is [`Palette::card`].
    pub surface: Color32,
    /// One step up from [`Palette::surface`] (in the light theme, a step
    /// toward the page): a button under the pointer, striped rows, a
    /// segmented control's chosen card.
    pub surface_raised: Color32,
    /// The ramp's far end: hover in a list, open menus over a panel.
    pub surface_hover: Color32,
    /// Hairlines that identify a control: widget frames, separators, indent
    /// guides.
    ///
    /// **Lightened on 2026-08-14, and the reason is WCAG 1.4.11.** A button at
    /// rest is [`Palette::surface_raised`] on [`Palette::surface`], ~1.2:1 of
    /// fill difference — so the hairline is the *only* channel that says "this
    /// is a control", and at 1.92:1 it was not saying it. This measures 3.8:1
    /// on the dark surface and clears the 3:1 floor for a UI component
    /// boundary; the light one is the hub's strong border taken a step darker
    /// (`#84979A` → `#788B8E`), which is what clears 3:1 on the light page.
    ///
    /// It is **solid, not white-at-an-alpha**: a hairline that has to clear a
    /// fixed ratio cannot be a translucent one, because what it composites over
    /// changes with the surface under it.
    pub line: Color32,
    /// The live tone — **teal**, `#2B837E` in both themes (since 2026-11-10;
    /// it was the bright `#5CC6C0` with dark ink on it). Selected segments,
    /// ticked boxes, sliders, the primary button and the plate an engaged
    /// control wears: the one saturated colour in the chrome, so it always
    /// means "this is the live thing". It carries [`Palette::on_accent`]
    /// (white) at 4.5:1, so anything filled with it is labelled in that.
    pub accent: Color32,
    /// The accent at the top of its ladder, as a **stroke**: the text cursor,
    /// a text-selection handle, the rim on a *held* control (north-up lock,
    /// caps lock). White in the dark theme, the hub's deep accent text in the
    /// light one.
    pub accent_bright: Color32,
    /// The fill an accent plate changes to under the pointer
    /// ([`primary_button_sized`]): a step deeper, so the white label still
    /// reads (5.1:1).
    pub accent_lift: Color32,
    /// The rim of an *accent-filled* plate: an engaged disc, a dock disc whose
    /// panel is open. A deeper teal than the plate, so the white glyph inside
    /// would read on it too (5.8:1). The rim of a pressed or hovered *button*
    /// is [`Palette::accent_word`] instead, which clears 3:1 on those fills.
    pub accent_ring: Color32,
    /// The accent, pushed *down* — the fill of an accent-filled button while
    /// the pointer is held on it. A press moves the opposite way from a hover,
    /// or the two states are one state. Its label is still
    /// [`Palette::on_accent`], at 5.8:1.
    pub accent_pressed: Color32,
    /// The top of the *text* ladder — [`Palette::text`] → [`Palette::cream`]
    /// → this. Links, speaker names, emphasised values, the open menu entry.
    /// White in the dark theme (emphasis is lightness there); the hub's deep
    /// teal link colour in the light one.
    pub accent_text: Color32,
    /// Words in the accent on a soft accent tag ([`Palette::accent_muted`]),
    /// the web's `--m-accent-text`: the pale [`Palette::accent_ring`] in the
    /// dark theme, the hub's deep teal in the light one (where the ring is too
    /// light to be read as type on the tag).
    pub accent_word: Color32,
    /// The soft half of the accent: a pressed button, a selected row, a text
    /// selection, an "on" tag. Dark, a deep teal that carries
    /// [`Palette::cream`] at 10.5:1; light, the hub's accent-soft.
    ///
    /// This is what keeps the ladder safe. `active.weak_bg_fill` carries a
    /// label drawn in egui's `strong_text_color`, which is
    /// [`Palette::cream`]; filling it with [`Palette::accent`] instead would
    /// put a light label on a light plate.
    pub accent_muted: Color32,
    /// The strongest text: headings and the one or two things that must read
    /// brightest (dark) or darkest (light). egui's `strong_text_color`.
    pub cream: Color32,
    /// Body text.
    pub text: Color32,
    /// Secondary text: units, coordinates, hints. Also `index.html`'s muted
    /// tone in the dark theme, so the pre-wasm splash and this one agree —
    /// **change it in both**.
    ///
    /// Lifted on 2026-08-14: this is the colour egui hands to
    /// `weak_text_color`, i.e. the one that lands on every surface in the ramp,
    /// and it measured 4.40:1 on the hover step and 4.12:1 inside an overlay box
    /// over bright map imagery. Both now clear 4.5.
    pub text_dim: Color32,
    /// Warnings. **One of the two colours the chrome keeps** besides the
    /// accent, for the reason road signs are not brand-coloured: caution and
    /// fault are statements about the world, not decoration. A desaturated gold
    /// on the dark theme, the hub's amber-brown on the light.
    pub warn: Color32,
    /// Errors. Warm rather than pure red so it belongs to the same palette;
    /// lifted from `#E88B7D` on 2026-08-14 (dark), because an error is the one
    /// string that must be readable wherever it lands.
    pub error: Color32,
    /// The fill of the boxes that float *over* the map (status HUD, stats,
    /// driver chrome, the dock, the round map buttons): [`Palette::panel`],
    /// solid.
    ///
    /// **Solid since 2026-11-08**, asked for that day: "a bad implementation of
    /// glass". What a box stands on is said by its shadow ([`card_shadow`],
    /// `Visuals::window_shadow`), not by the map showing through
    /// (`monochrome-chrome.md` has the glass's history). **The same as a
    /// panel**, also asked that day: "some panels are lighter than others; they
    /// must be dark". One tone for every surface a person reads as a panel;
    /// only a card *inside* one stands off it.
    pub overlay: Color32,
    /// The window and popup fill: [`Palette::overlay`]'s tone. The name stays
    /// for the call sites that paint "the card a window is".
    pub glass: Color32,
    /// Every panel's surface — docked, full-screen on a phone, floating over
    /// the map.
    ///
    /// **Dark: found with the person, 2026-11-10.** At the ink the panels
    /// "looked black"; a teal pressed nearly to black (`#0B1918`) was "too
    /// little colour", and the person pointed at the midas hub's question card
    /// (`#192627`). With that, and surface wells in it as the hub's options
    /// are, the blocks had "too much contrast against the background" — so this
    /// is the hub's teal a step down, and the cards inside are
    /// [`Palette::card`], close to it. **Light:** the hub's page, with white
    /// cards on it.
    pub panel: Color32,
    /// A card inside a panel ([`list_group`], a chat or contact row). Dark:
    /// just off [`Palette::panel`], so a group of rows reads as one block by
    /// its hairline and shadow rather than by a jump in tone. Light: white.
    pub card: Color32,
    /// The hairline round a panel, a window or a box over the map: the midas
    /// hub's `--m-border`, solid. The shadow does the lifting; this only keeps
    /// a card's edge.
    pub panel_rim: Color32,
    /// The rim around a box or a round button over the map:
    /// [`Palette::panel_rim`]. It never identifies a *control* — that is
    /// [`Palette::line`]'s job.
    pub overlay_line: Color32,
    /// The fill of a control that is a *guide* rather than a plate — the
    /// thumbstick's well, which at [`Palette::overlay`]'s opacity read as a hole
    /// punched in the map rather than as something to put a thumb on. Half the
    /// coverage; nothing legible sits on it. The floor is the other direction:
    /// at alpha 90 the well vanished into pale beige map.
    pub overlay_soft: Color32,
    /// The rule between the rows of a [`list_group`]: the text tone at a low
    /// alpha, so it is a hairline on any card.
    pub list_line: Color32,
    /// How dark the shadows are, as a factor on their alpha: a light page
    /// takes the hub's faint lift, not the dark theme's deep one.
    pub shadow: f32,
    /// The fill of a tile or card that is *on* (a layer tile): the soft teal
    /// in the dark theme; white in the light one, where the soft teal read
    /// as washed out (2026-11-10) and the accent rim and icon say "on".
    pub tile_on: Color32,
    /// The knob of a switch that is on, over its accent track: dark ink in
    /// the dark theme, white in the light one, as a light UI draws a switch.
    pub knob_on: Color32,
    /// A count on a round button (the layer categories): the midas hub's gold,
    /// its one accent besides the teal, with [`Palette::on_gold`] on it. It was
    /// a cream or near-black disc, which read as a black dot (2026-11-10).
    pub gold: Color32,
    /// The ink on [`Palette::gold`].
    pub on_gold: Color32,
    /// The hairline round a button or a field at rest: the midas hub's soft
    /// border, a step firmer. It was [`Palette::line`], a 3:1 grey, and a page
    /// of buttons in it read as "cheap" boxes (2026-11-10); a control is now
    /// told by its plate, its shape and its word, as in the hub, with
    /// [`Palette::line`] kept for what must be found by its edge alone.
    pub control_edge: Color32,
    /// Whether egui's dark visuals are the base ([`visuals`]).
    pub dark: bool,
}

/// Today's look: the deep teal, read off Android's keyboard in its teal dark
/// theme (2026-10-10) and laid out as the midas hub lays out its dark mode.
pub const DARK: Palette = Palette {
    ink: Color32::from_rgb(0x0F, 0x14, 0x16),
    on_accent: Color32::WHITE,
    on_status: Color32::from_rgb(0x0F, 0x14, 0x16),
    surface: Color32::from_rgb(0x14, 0x1A, 0x1C),
    surface_raised: Color32::from_rgb(0x1F, 0x28, 0x29),
    surface_hover: Color32::from_rgb(0x2A, 0x35, 0x36),
    line: Color32::from_rgb(0x63, 0x79, 0x7C),
    accent: Color32::from_rgb(0x2B, 0x83, 0x7E),
    accent_bright: Color32::WHITE,
    accent_lift: Color32::from_rgb(0x27, 0x7A, 0x75),
    accent_ring: Color32::from_rgb(0x24, 0x70, 0x6C),
    accent_pressed: Color32::from_rgb(0x24, 0x70, 0x6C),
    accent_text: Color32::WHITE,
    accent_word: Color32::from_rgb(0xA8, 0xDC, 0xD8),
    accent_muted: Color32::from_rgb(0x26, 0x3F, 0x42),
    cream: Color32::from_rgb(0xF4, 0xF8, 0xF8),
    text: Color32::from_rgb(0xE2, 0xE9, 0xEA),
    text_dim: Color32::from_rgb(0xAA, 0xC2, 0xC6),
    warn: Color32::from_rgb(0xD8, 0xC0, 0x7A),
    error: Color32::from_rgb(0xF2, 0xA2, 0x96),
    overlay: Color32::from_rgb(0x17, 0x23, 0x24),
    glass: Color32::from_rgb(0x17, 0x23, 0x24),
    panel: Color32::from_rgb(0x17, 0x23, 0x24),
    card: Color32::from_rgb(0x16, 0x1F, 0x21),
    panel_rim: Color32::from_rgb(0x2A, 0x35, 0x36),
    overlay_line: Color32::from_rgb(0x2A, 0x35, 0x36),
    overlay_soft: Color32::from_rgba_unmultiplied_const(0x14, 0x1A, 0x1C, 140),
    list_line: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 22),
    shadow: 1.0,
    tile_on: Color32::from_rgb(0x26, 0x3F, 0x42),
    knob_on: Color32::from_rgb(0xE2, 0xE9, 0xEA),
    gold: Color32::from_rgb(0xD8, 0xC0, 0x7A),
    on_gold: Color32::from_rgb(0x1F, 0x28, 0x29),
    control_edge: Color32::from_rgb(0x3A, 0x48, 0x49),
    dark: true,
};

/// The midas hub's light theme (2026-11-10), role for role. Three values move
/// off the hub's, each for a ratio a test holds:
///
///   - [`Palette::accent`] is the hub's `#5CC6C0` taken down to `#34938E`, the
///     same hue: the hub's accent is 2.0:1 on white, and a plate, a slider's
///     fill or a hover ring that identifies a control needs 3:1 against the
///     surface it sits on (`a_control_is_distinguishable_from_the_surface_it_sits_on`).
///     The hub's own value is what a plate lifts to under the pointer
///     ([`Palette::accent_lift`]), so it is still seen.
///   - [`Palette::line`] is its strong border a step darker (3:1 on the page).
///   - [`Palette::accent_ring`] / [`Palette::accent_pressed`] are a deeper teal
///     than its accent-hover (3:1 round a pressed tag, 4.5:1 under the
///     plate's ink).
pub const LIGHT: Palette = Palette {
    ink: Color32::from_rgb(0xE7, 0xED, 0xED),
    on_accent: Color32::WHITE,
    on_status: Color32::WHITE,
    surface: Color32::WHITE,
    surface_raised: Color32::from_rgb(0xF8, 0xFA, 0xFA),
    surface_hover: Color32::from_rgb(0xE7, 0xED, 0xED),
    line: Color32::from_rgb(0x78, 0x8B, 0x8E),
    accent: Color32::from_rgb(0x2B, 0x83, 0x7E),
    accent_bright: Color32::from_rgb(0x17, 0x69, 0x6A),
    accent_lift: Color32::from_rgb(0x27, 0x7A, 0x75),
    accent_ring: Color32::from_rgb(0x24, 0x70, 0x6C),
    accent_pressed: Color32::from_rgb(0x24, 0x70, 0x6C),
    accent_text: Color32::from_rgb(0x17, 0x69, 0x6A),
    accent_word: Color32::from_rgb(0x17, 0x69, 0x6A),
    accent_muted: Color32::from_rgb(0xD9, 0xF0, 0xEE),
    cream: Color32::from_rgb(0x0F, 0x14, 0x16),
    text: Color32::from_rgb(0x1F, 0x28, 0x29),
    text_dim: Color32::from_rgb(0x54, 0x65, 0x68),
    warn: Color32::from_rgb(0x85, 0x57, 0x00),
    error: Color32::from_rgb(0xB3, 0x36, 0x1E),
    overlay: Color32::from_rgb(0xF1, 0xF5, 0xF5),
    glass: Color32::from_rgb(0xF1, 0xF5, 0xF5),
    panel: Color32::from_rgb(0xF1, 0xF5, 0xF5),
    card: Color32::WHITE,
    panel_rim: Color32::from_rgb(0xDA, 0xE3, 0xE3),
    overlay_line: Color32::from_rgb(0xDA, 0xE3, 0xE3),
    overlay_soft: Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 150),
    list_line: Color32::from_rgba_unmultiplied_const(0x0F, 0x14, 0x16, 22),
    shadow: 0.6,
    tile_on: Color32::WHITE,
    knob_on: Color32::WHITE,
    gold: Color32::from_rgb(0xE0, 0xBC, 0x5C),
    on_gold: Color32::from_rgb(0x1F, 0x28, 0x29),
    control_edge: Color32::from_rgb(0xC9, 0xD4, 0xD5),
    dark: false,
};

static DARK_PALETTE: Palette = DARK;
static LIGHT_PALETTE: Palette = LIGHT;
static LIGHT_ON: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Set while [`dark_here`] draws something that is dark in both themes.
    static DARK_HERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The palette in force. Read it where a colour is painted, never into a
/// `const`, a `static` or a cached field: a copy taken before [`set_light`]
/// keeps the other theme's colour.
#[must_use]
pub fn p() -> &'static Palette {
    if LIGHT_ON.load(Ordering::Relaxed) && !DARK_HERE.with(std::cell::Cell::get) {
        &LIGHT_PALETTE
    } else {
        &DARK_PALETTE
    }
}

/// Draw `add` in the dark palette whatever the theme: a screen that is dark in
/// both, as a phone's call and camera screens are (a picture, or the dark
/// stage a call shows before one arrives). [`p`] answers [`DARK`] on this
/// thread until it returns; give each `Ui` drawn inside it
/// [`use_dark_visuals`] too, since egui's own widgets read the style, not
/// [`p`].
pub fn dark_here<R>(add: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            DARK_HERE.with(|d| d.set(self.0));
        }
    }
    let _restore = Restore(DARK_HERE.with(|d| d.replace(true)));
    add()
}

/// Give `ui` (and what it lays out) the dark visuals, for a surface drawn in
/// [`dark_here`]. Nothing to do while the theme is dark.
pub fn use_dark_visuals(ui: &mut egui::Ui) {
    if is_light() {
        ui.style_mut().visuals = visuals_of(&DARK);
    }
}

/// Switch the chrome to the light theme or back. Only the palette: the egui
/// style that holds its colours is re-applied by [`apply_visuals`], which
/// `ui_theme::follow_the_light` calls when the answer changes.
pub fn set_light(on: bool) {
    LIGHT_ON.store(on, Ordering::Relaxed);
}

/// Is the light theme in force?
#[must_use]
pub fn is_light() -> bool {
    LIGHT_ON.load(Ordering::Relaxed)
}

/// **The brand orange, kept for the brand.**
///
/// Its scope is the logo — the pin of [`paint_mark`], the tail
/// of the wordmark ([`WORDMARK_TAIL_COLOR`]) and, in `index.html`, the
/// loader's mark and progress bar — plus the few things that must shout: an
/// unread badge, the live dot. The live *controls* are [`Palette::accent`], its softer
/// sibling, and must not use this.
///
/// The separation is the point. While the chrome was orange this was spelled
/// `= ACCENT`, so the logo and every live control in the app were one constant —
/// and repainting the chrome would have repainted the logo with it, silently.
pub const BRAND: Color32 = Color32::from_rgb(0xFD, 0x60, 0x12);

/// A person's **real location** — where their phone is, shared with a private
/// chat (`systems::private_chat`). Blue because every map app draws the phone
/// that way, and never the brand orange, which marks places sent in a chat: the
/// two must not be confused on the map, one being a spot somebody pointed at and
/// the other being where somebody actually is. Used for nothing else.
pub const REAL_LOCATION: Color32 = Color32::from_rgb(0x4F, 0xB2, 0xFF);

/// **Public, and on the fediverse**: a community's posts and anything that
/// came from another server (`systems::community_posts`). Lavender, the colour
/// the design gave it (`docs/mockups/communities/v4-public-posts.html`), so the
/// public half of a community never reads like its encrypted topics. Used for
/// nothing else.
pub const PUBLIC: Color32 = Color32::from_rgb(0xB3, 0x9D, 0xFF);

/// **The council**: an item of a municipal council's business on the Plans
/// layer (`systems::plans`), the square on the map and its badge on a card.
/// The design's deep blue (`docs/mockups/plans-and-decisions/v1.png`), away
/// from the brand orange the layer uses for notices still open. Used for
/// nothing else.
pub const COUNCIL: Color32 = Color32::from_rgb(0x2F, 0x4A, 0x7A);
/// The council square's rim and the council's text on a dark card: the same
/// blue, light enough to read.
pub const COUNCIL_LIGHT: Color32 = Color32::from_rgb(0x9D, 0xB7, 0xE6);

// Script editor syntax colours (`lua_highlight`), on the code box's ink —
// [`DARK`]'s in both themes: the script editor keeps a dark code box in the
// light theme, so these are one set. Their own constants rather than borrowed
// ones: a warning tone on every string would read as a page of warnings.
/// Keywords — the accent, so the structure of a script is what stands out.
pub const CODE_KEYWORD: Color32 = Color32::from_rgb(0x5C, 0xC6, 0xC0);
/// String literals — a warm sand.
pub const CODE_STRING: Color32 = Color32::from_rgb(0xE6, 0xB8, 0x86);
/// Numbers and `true` / `false` / `nil`.
pub const CODE_LITERAL: Color32 = Color32::from_rgb(0xC3, 0xA9, 0xFF);
/// Comments — recede, but stay readable.
pub const CODE_COMMENT: Color32 = Color32::from_rgb(0x86, 0x9C, 0xA0);
/// The host's own functions and handlers.
pub const CODE_API: Color32 = Color32::from_rgb(0x8F, 0xCB, 0xFF);
/// Any other call.
pub const CODE_CALL: Color32 = Color32::from_rgb(0xD6, 0xE4, 0xA8);

/// Backing for the small text chips painted straight onto the map — place-pin
/// labels, the drag ghost, a feature's hover card. Darker and more opaque than
/// [`Palette::overlay`] on purpose: these are a few words over an arbitrary tile, so
/// legibility beats letting the map through.
pub const LABEL_CHIP: Color32 = Color32::from_rgba_unmultiplied_const(0x0F, 0x14, 0x16, 235);
/// Halo behind map text that has no chip to sit on — the street names
/// (`systems::street_labels`), which are painted *along* their street and would
/// black it out under a chip. **Dark**, and only mostly opaque: a street name
/// lies on the street, and a street is tarmac — dark grey — so the halo sinks
/// into the road and the pale [`LABEL_INK`] reads as lettering painted on it.
/// Over a pale square or water it surfaces as a soft shadow that keeps the
/// letters lifted. The pair used to run the other way (near-black ink on a pale
/// halo), and on tarmac the eye saw only the halo: a white outline round dark
/// letters, which read as a sticker on the road rather than a name in it.
/// Neutral, not tinted: the map underneath supplies all the hue in the frame.
pub const LABEL_HALO: Color32 = Color32::from_rgba_unmultiplied_const(0x16, 0x17, 0x19, 190);
/// The stroke inside [`LABEL_HALO`]: a warm off-white, the colour of road paint
/// rather than of paper, and not [`Palette::cream`] — this one lies on tarmac in the
/// world, that one is the chrome's, and the two are free to move apart.
pub const LABEL_INK: Color32 = Color32::from_rgb(0xF6, 0xF3, 0xEC);

/// A scenario over the map (`systems::scenario`, `docs/mockups/scenarios/`).
/// A region's own colour is the scenario file's (it is data, like a legend);
/// what is here is the frame each mode wears and the inks of a track.
pub mod scenario {
    use egui::Color32;

    /// Live: an official source only, so the one frame allowed to look like a
    /// warning (v4-whole).
    pub const LIVE: Color32 = Color32::from_rgb(0xD6, 0x3A, 0x2F);
    pub const REPLAY: Color32 = Color32::from_rgb(0x4C, 0xAF, 0x6E);
    /// Exercise: the yellow of its stripes, on every screen.
    pub const EXERCISE: Color32 = Color32::from_rgb(0xE8, 0xC5, 0x47);
    pub const SANDBOX: Color32 = Color32::from_rgb(0x9B, 0x7B, 0xFF);

    /// A track's line, behind and (dashed) ahead, and its time ticks.
    pub const TRACK: Color32 = Color32::from_rgb(0xFF, 0xCF, 0x9A);
    /// The soft line under the path behind.
    pub const TRACK_GLOW: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0xB2, 0x7A, 40);
    /// The cone of where it may go.
    pub const CONE: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0x7A, 0x4A, 34);
    pub const CONE_EDGE: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0x9A, 0x6A, 90);
    /// The line between two areas of a figure: dark and thin, so neighbours
    /// in the same band still read as two places.
    pub const AREA_EDGE: Color32 = Color32::from_rgba_unmultiplied_const(0x12, 0x18, 0x1A, 150);
    /// The dark band under the light outline of the place being read, so it
    /// holds on a pale fill as on a dark one.
    pub const AREA_HALO: Color32 = Color32::from_rgba_unmultiplied_const(0x0F, 0x14, 0x16, 170);
    /// The glow round a group's head, and its members.
    pub const HEAD: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0x6A, 0x3A, 90);
    pub const HEAD_DOT: Color32 = Color32::from_rgb(0xFF, 0xF1, 0xC9);
    /// The rim of a group's label chip.
    pub const HEAD_RIM: Color32 = Color32::from_rgb(0xFF, 0x8A, 0x5A);
    /// Where some were brought down.
    pub const INTERCEPT: Color32 = Color32::from_rgb(0x9F, 0xD3, 0xFF);
}

// ─── Fonts ────────────────────────────────────────────────────────────────────

/// The wordmark face (SIL OFL — see `assets/fonts/README.md`). Embedded rather
/// than loaded as an asset because the splash draws on frame one, before any
/// asset pipeline has run — and on the web that would be an HTTP round trip.
/// egui reads TTF/OTF, not woff2.
/// Also the face the street names on the map are rasterised in
/// (`systems::street_labels`): a semibold reads at the size a road marking gets,
/// where egui's light body face breaks up.
pub const BRAND_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/PlusJakartaSans-SemiBold.ttf"
));
const BRAND_FAMILY: &str = "cartopolis-brand";

/// The body face: Plus Jakarta Sans **Regular**, the brand face's own family
/// one weight down, at the front of egui's `Proportional` stack.
///
/// **Until 2026-10-04 body text was egui's default, Ubuntu Light**, which is
/// a fine debugging face and a poor one for this UI: a light weight in light
/// ink on a dark panel thins out at the 11–12.5 pt the panels are full of,
/// and next to a Jakarta heading it was a second, unrelated typeface. One
/// family at two weights is what makes a heading and its rows read as one
/// voice. Ubuntu stays behind it in the stack for the glyphs Jakarta lacks.
const BODY_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/PlusJakartaSans-Regular.ttf"
));
const BODY_FAMILY: &str = "cartopolis-body";

/// Noto Emoji, one-colour, Unicode 15, at the **end** of every stack. egui's
/// own `NotoEmoji-Regular` is an old subset that stops at U+1F6C5, so the
/// emoji people actually type in chat (🙂 🕺 🤖 🛒) drew as `◻` — on the phone
/// too. Last, so every glyph something earlier in the stack already drew
/// keeps its look; colour emoji cannot be drawn by epaint at all.
const EMOJI_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/NotoEmoji-Regular.ttf"
));
const EMOJI_FAMILY: &str = "cartopolis-emoji";

/// The brand face: Plus Jakarta Sans SemiBold, backed by egui's own stack so a
/// glyph it lacks (emoji, arrows, the box-drawing the panels use) still resolves
/// instead of rendering as tofu. Used for headings and the splash wordmark —
/// *not* for body text, which this weight would make heavy and hard to read.
pub fn brand_family() -> FontFamily {
    FontFamily::Name(BRAND_FAMILY.into())
}

fn fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        BRAND_FAMILY.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(BRAND_TTF)),
    );
    // The icon face (`systems::icons`). Its codepoints are all private-use, so it
    // can never shadow a character another font in the stack would have drawn —
    // which is why it is appended to *every* family rather than kept in one of
    // its own: an icon has to render wherever a label does, including inside a
    // heading in the brand family and beside a monospace readout.
    fonts.font_data.insert(
        icons::FONT_NAME.to_owned(),
        std::sync::Arc::new(
            egui::FontData::from_static(icons::FONT).tweak(egui::FontTweak {
                // Material Symbols draws on a full em square (advance 1.0 em, ink
                // from −0.1 to 0.88 em), so at the *same* point size as the text
                // beside it an icon reads a size too big, sits a touch high, and
                // raises the row height of every line it appears in. These two
                // numbers are the whole optical fit — `docs/notes/icon-font.md`
                // has where they come from and when to re-check them.
                scale: 1.15,
                y_offset_factor: 0.03,
                ..Default::default()
            }),
        ),
    );
    fonts.font_data.insert(
        EMOJI_FAMILY.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(EMOJI_TTF)),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        if let Some(stack) = fonts.families.get_mut(&family) {
            stack.push(EMOJI_FAMILY.to_owned());
            stack.push(icons::FONT_NAME.to_owned());
        }
    }
    // The body face goes in front of the proportional stack (see [`BODY_TTF`]),
    // before the brand stack below copies it.
    fonts.font_data.insert(
        BODY_FAMILY.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(BODY_TTF)),
    );
    if let Some(stack) = fonts.families.get_mut(&FontFamily::Proportional) {
        stack.insert(0, BODY_FAMILY.to_owned());
    }
    // Prepend to a *copy* of the proportional stack rather than pushing onto it:
    // the brand face wins where it has the glyph, egui's fallbacks cover the rest,
    // and `Proportional` itself is left alone so body text keeps the body face.
    let mut stack = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    stack.insert(0, BRAND_FAMILY.to_owned());
    fonts.families.insert(brand_family(), stack);
    fonts
}

// ─── Type scale ───────────────────────────────────────────────────────────────

/// Captions and badges, in points: the word under a control, the initial in a
/// profile dot, a unit. The floor — nothing in the app should be smaller, and
/// three sizes below body (9.5, 10, 10.5) all meant this one.
pub const SIZE_TINY: f32 = 10.0;
/// Chips and switchers: `Auto`/`Day`/`Night`, a coordinate readout in an overlay
/// box, anything that is a *setting* or a value rather than a sentence. Also what
/// [`egui::TextStyle::Small`] resolves to, so `RichText::small()` agrees with it.
pub const SIZE_CHIP: f32 = 11.0;
/// The secondary line under a row's title: an address under a place name, a hint
/// under a field, a status under a heading. **This is the one that drifted** —
/// it was 13.0 in Layers, 12.5 in Route, 12.0 in the sign-in card and 11.0 in the
/// HUD, four answers to one question.
pub const SIZE_SUB: f32 = 12.5;
/// Body text — the same as [`egui::TextStyle::Body`] and `Button`, i.e. what a
/// plain `ui.label` already gets. Spell it out only inside a `RichText` chain
/// that has to state a size; otherwise leave the size off entirely.
pub const SIZE_BODY: f32 = 14.0;
/// The one number a box is *about*: the HUD's clock, a primary button's label
/// ([`primary_button_sized`]). One per box — a second is just body text.
pub const SIZE_VALUE: f32 = 15.0;
/// A panel header's title, in the brand face ([`panel_header`]).
pub const SIZE_TITLE: f32 = 17.0;
/// [`egui::TextStyle::Heading`], in the brand face.
pub const SIZE_HEADING: f32 = 19.0;
/// A panel's own name in its title bar: the Chats screen's large title, so
/// every panel opens the same way.
pub const SIZE_PANEL_TITLE: f32 = 20.5;
/// [`egui::TextStyle::Monospace`] — the code-ish body size. Monospace runs
/// visually larger than proportional at the same point size, which is why this is
/// a point under [`SIZE_BODY`] rather than equal to it.
pub const SIZE_MONO: f32 = 13.0;

/// The splash wordmark. Not a scale entry: it exists exactly twice, here and in
/// `index.html`, which has to mirror it (see `systems::splash`'s module note).
pub const SPLASH_WORDMARK: f32 = 40.0;
/// The motto under the wordmark. Mirrored in `index.html` for the same reason.
pub const SPLASH_MOTTO: f32 = 16.0;

// ─── Corner radius ────────────────────────────────────────────────────────────

/// The corner radius: buttons, menus, chips, the panel header band. **Reach for
/// this one unless there is a reason not to** — a UI that rounds eight ways (the
/// number this file's single private constant was competing with) reads as eight
/// UIs.
///
/// Public because the alternative is what actually happened: a caller that needed
/// the app's radius read it back off `visuals.widgets.inactive.corner_radius`.
///
/// **The whole scale was opened up with the move to glass** (6 → 8, 3 → 4,
/// 10 → 14). A translucent surface is defined by its edge rather than by its
/// fill, and a tight corner on a sheet you can see through reads as a cut-out;
/// the softer radius is half of what makes the chrome look like a pane rather
/// than a rectangle. It costs nothing else — the radii are only ever read
/// through these four names.
pub const RADIUS: u8 = 8;
/// Small: the checkbox square, the close button, a label chip painted over the
/// map. For anything under ~24 pt, where [`RADIUS`] is half the height and the
/// box stops reading as a box (an unchecked checkbox becomes a radio button).
/// Held down at 4 while the others opened up, for that reason: on a ~14 pt
/// checkbox even [`RADIUS`] is already half the height.
pub const RADIUS_SM: u8 = 4;
/// Large: windows, cards, and the boxes that float over the map
/// ([`overlay_frame`]). A bigger surface needs a bigger radius to look equally
/// rounded — the same value at two sizes does not.
pub const RADIUS_LG: u8 = 14;
/// A pill: fully rounded whatever the height. epaint clamps a corner radius to
/// half the shortest side, so this is "as round as it can be" rather than a
/// pixel count — which is what makes it safe on a box of any size.
pub const RADIUS_PILL: u8 = u8::MAX;

// ─── Touch targets ────────────────────────────────────────────────────────────

/// Material's minimum touch target, in dp (density-independent pixels, 1/160 in
/// — about 7.6 mm, roughly a fingertip). Every platform's guideline is within a
/// millimetre of this one.
///
/// The app was under it everywhere a finger drives: [`style`]'s `interact_size`
/// is 24 pt tall and 24 pt is ~36 dp on the phone tier, and the close button was
/// 22 pt (~33 dp). Raising the *UI scale* is not the fix — past ~1.7× the
/// viewport drops below `responsive_right_panel`'s 240 pt floor and a docked
/// panel is wider than the screen it docks in — so the interactive sizes are
/// raised on the touch tier instead, and this is the number they are raised to.
pub const MIN_TOUCH_TARGET_DP: f32 = 48.0;

/// `index.html`'s `MAX_DPR` on the phone tier — device pixels per CSS pixel the
/// render budget will draw (`docs/notes/wasm-performance.md`). It matters here
/// because it is *also* the magnification the stage transform applies to every
/// egui point on the web, i.e. the thing that decides how big a point physically
/// is. Duplicated rather than plumbed, because the page decides it before any
/// Rust exists; if it changes there, [`dp_per_point`] changes with it.
pub const WEB_PHONE_MAX_DPR: f32 = 1.5;

/// How many dp one egui point covers — the only way to size anything in a
/// physical unit from inside this process.
///
///   - **Native (and Android):** winit's scale factor *is* the display density,
///     so one window point is one dp and an egui point is `ui_scale` of those.
///     → `ui_scale`.
///   - **Web:** a CSS pixel is a dp on a phone, but the render budget sizes the
///     canvas to viewport ÷ k and scales it back up by k, so one window point
///     covers k CSS px. → `ui_scale · k`.
///
/// `k` is `max(1, dpr / MAX_DPR, √(pixel budget))` in `index.html` and only the
/// middle term is reconstructible from here, so this **under**-estimates k —
/// which over-estimates how many points 48 dp is, making targets slightly larger
/// than the guideline rather than smaller. That is the safe direction for a
/// floor, and the reason this is not plumbed through from JS for a number that is
/// only accurate to a rounding anyway.
///
/// Lives here rather than beside any one widget because the dock discs, the close
/// button, the driver rail and the on-screen keyboard all ask the same question,
/// and `onscreen_keyboard.rs` having had to compute its own is how a second
/// answer gets into the tree.
#[must_use]
pub fn dp_per_point(ui_scale: f32, display_scale: f32, web_phone: bool) -> f32 {
    let stage_k = if web_phone {
        (display_scale / WEB_PHONE_MAX_DPR).max(1.0)
    } else {
        1.0
    };
    (ui_scale * stage_k).max(0.1)
}

/// [`MIN_TOUCH_TARGET_DP`] converted to points, given a [`dp_per_point`].
#[must_use]
pub fn min_touch_target_pts(dp_per_point: f32) -> f32 {
    MIN_TOUCH_TARGET_DP / dp_per_point.max(0.1)
}

/// Does this run lay out for a finger?
///
/// [`crate::quality`] is deliberately the *one* detector in this client (see its
/// module note): the browser decides once, in `index.html`, because the canvas
/// resolution is a page-layout fact Rust cannot reach, and a second media query
/// here is how the UI ends up finger-sized on a machine rendering at desktop
/// resolution.
#[must_use]
pub fn touch_layout() -> bool {
    crate::quality::is_phone()
}

/// Guaranteed height of one interactive row (`interact_size.y`), pointer tier.
/// 28 since 2026-11-08 (was 24): "the menu items are too cramped".
const ROW_PTS: f32 = 28.0;
/// The same on the touch tier. The floor is [`MIN_TOUCH_TARGET_DP`] at the
/// tier's own [`dp_per_point`], which is ≥1.5 on both platforms that reach it
/// (native Android's `phone_ui_scale`, and the web's stage transform): 32 pt is
/// 48 dp at exactly 1.5. It is 40 since 2026-11-08, above the floor, for room
/// rather than reach.
pub const TOUCH_ROW_PTS: f32 = 40.0;

/// The height of one interactive row in this run, in points. Match it whenever
/// you hand-place a hit area (`allocate_exact_size`) instead of adding a widget —
/// that is the path egui's `interact_size` cannot reach for you.
#[must_use]
pub fn row_height() -> f32 {
    if touch_layout() {
        TOUCH_ROW_PTS
    } else {
        ROW_PTS
    }
}

// ─── Style ────────────────────────────────────────────────────────────────────

fn widgets(c: &Palette) -> Widgets {
    let visuals = |bg: Color32, line: Color32, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0, line),
        fg_stroke: Stroke::new(1.0, fg),
        corner_radius: CornerRadius::same(RADIUS),
        expansion: 0.0,
    };
    Widgets {
        // Separators, indent guides and plain labels all read off this one.
        noninteractive: visuals(c.surface, c.line, c.text),
        // At rest and under the pointer as the midas hub's `m-button`: a dark
        // surface that lifts a step under the pointer. The hub's hairline is
        // quieter than [`Palette::line`]; this one keeps the 3:1 a control boundary
        // needs (`a_control_is_distinguishable_from_the_surface_it_sits_on`).
        inactive: visuals(c.surface, c.control_edge, c.text),
        hovered: visuals(c.surface_raised, c.accent_word, c.cream),
        // Pressed / focused — **one fill, not two.** `weak_bg_fill` (the button
        // fill) carries a [`Palette::cream`] label, so both fills are the deep
        // [`Palette::accent_muted`], which carries [`Palette::cream`] at 10.5:1
        // (dark) and looks the same whether a control is a button or a
        // checkbox; its rim is [`Palette::accent_word`], clear of that fill.
        //
        // `fg_stroke` must stay bright regardless: egui derives
        // `Visuals::strong_text_color` from it, so *every* `RichText::strong()` in
        // the app is this colour. Setting it to ink to suit a light fill would
        // turn every bold value in the UI near-invisible.
        active: WidgetVisuals {
            expansion: 1.0,
            ..visuals(c.accent_muted, c.accent_word, c.cream)
        },
        // An open menu button keeps the hover surface but drops the bright ring —
        // the popup below it is the thing to look at now.
        open: visuals(c.surface_hover, c.line, c.cream),
    }
}

/// The full brand [`egui::Visuals`] for the palette in force ([`p`]).
#[must_use]
pub fn visuals() -> egui::Visuals {
    visuals_of(p())
}

/// The brand [`egui::Visuals`] for one palette, built by overriding
/// [`egui::Visuals::dark`] (or [`egui::Visuals::light`]) rather than from
/// scratch so the fields this doesn't have an opinion about (notably the
/// font colour transfer function) stay at egui's tuned defaults.
#[must_use]
pub fn visuals_of(c: &Palette) -> egui::Visuals {
    let mut v = if c.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.widgets = widgets(c);
    v.selection = Selection {
        bg_fill: c.accent_muted,
        stroke: Stroke::new(1.0, c.cream),
    };
    v.hyperlink_color = c.accent_text;
    v.faint_bg_color = c.surface_raised;
    v.extreme_bg_color = c.ink;
    // A text field is a card, not a well: an ink well read as a black slot in
    // the dark theme (2026-11-10).
    v.text_edit_bg_color = Some(c.card);
    v.code_bg_color = c.ink;
    v.warn_fg_color = c.warn;
    v.error_fg_color = c.error;
    // Pin the weak/disabled tone instead of letting egui derive it by alpha —
    // this is the same grey-blue the two splashes use, and it is measured.
    v.weak_text_color = Some(c.text_dim);
    // Disabled is the one tone egui still derived by alpha, and its default 0.5
    // put a disabled label at 4.1:1 on the panel — *dimmer than `TEXT_DIM` hint
    // text at 6.9:1*, so the sentence explaining why a control was unavailable
    // was brighter than the control. It also took a button's frame to 1.1:1
    // against the panel, i.e. gone. 0.65 keeps disabled clearly below the live
    // state without making it a colour nobody can find.
    v.disabled_alpha = 0.65;

    // The midas hub's layers: a page ([`Palette::panel`]) and cards on it ([`Palette::glass`],
    // which is [`Palette::surface`]), solid.
    v.panel_fill = c.panel;
    v.window_fill = c.glass;
    // The hub's hairline. The same one the boxes over the map wear: a docked
    // panel and a floating readout are the same material at two sizes.
    v.window_stroke = Stroke::new(1.0, c.panel_rim);
    v.window_corner_radius = CornerRadius::same(RADIUS_LG);
    // The popups are the cards over the map (a place, a building), so they take
    // the windows' radius: at 8 pt beside 14 pt panes they read as a
    // different, cheaper material (2026-10-31).
    v.menu_corner_radius = CornerRadius::same(RADIUS_LG);
    // The hub's `--lift-2`, as near as one egui shadow comes: soft, a little
    // below. These float over a moving map, and a tight hard shadow reads as a
    // sticker rather than a raised surface.
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha((150.0 * c.shadow) as u8),
    };
    v.popup_shadow = Shadow {
        offset: [0, 8],
        blur: 22,
        spread: 0,
        color: Color32::from_black_alpha((125.0 * c.shadow) as u8),
    };

    v.text_cursor.stroke = Stroke::new(2.0, c.accent_bright);
    // The filled part of a slider takes the accent, so a value reads at a glance.
    v.slider_trailing_fill = true;
    v
}

/// The full brand [`egui::Style`] — [`visuals`] plus the spacing and type scale,
/// sized for this run's pointer ([`touch_layout`]).
pub fn style() -> egui::Style {
    style_for(touch_layout())
}

/// [`style`] with the pointer stated rather than detected, so the touch tier's
/// sizing is testable on a workstation.
pub fn style_for(touch: bool) -> egui::Style {
    let mut style = egui::Style {
        visuals: visuals(),
        ..Default::default()
    };

    let s = &mut style.spacing;
    // The "cramped" fix is mostly here. egui's defaults are 3 pt between rows and
    // vec2(4, 1) of button padding; those are inspector values, and a panel of
    // place rows built on them has no rhythm at all.
    // Raised from 6 on 2026-11-08 ("make the UI feel less cramped").
    s.item_spacing = egui::vec2(8.0, 8.0);
    // The touch tier is not "the same UI, bigger" — the *type* stays where it is
    // (it is already sharp on a phone; see `egui_manager::phone_ui_scale`) and
    // only the parts a finger has to hit grow, to [`MIN_TOUCH_TARGET_DP`]. That
    // is the whole difference between the two tiers.
    s.button_padding = if touch {
        egui::vec2(14.0, 10.0)
    } else {
        egui::vec2(12.0, 6.0)
    };
    // [`ROW_PTS`] of guaranteed height per clickable row on a pointer (egui
    // ships 18), [`TOUCH_ROW_PTS`] under a finger.
    s.interact_size = egui::vec2(40.0, if touch { TOUCH_ROW_PTS } else { ROW_PTS });
    s.window_margin = Margin::same(14);
    s.menu_margin = Margin::same(10);
    s.menu_spacing = 6.0;
    s.indent = 20.0;
    s.icon_spacing = 6.0;
    s.slider_width = 120.0;
    s.combo_width = 120.0;
    // A 10 pt scrollbar is a 15 dp drag handle; a finger needs the same 48 dp
    // everything else does, and a bar it cannot grab is a panel that only scrolls
    // by fling.
    s.scroll.bar_width = if touch { 16.0 } else { 10.0 };

    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(SIZE_CHIP, FontFamily::Proportional),
        ),
        (
            TextStyle::Body,
            FontId::new(SIZE_BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Button,
            FontId::new(SIZE_BODY, FontFamily::Proportional),
        ),
        // The one place the brand face appears in the app chrome.
        (
            TextStyle::Heading,
            FontId::new(SIZE_HEADING, brand_family()),
        ),
        (
            TextStyle::Monospace,
            FontId::new(SIZE_MONO, FontFamily::Monospace),
        ),
    ]
    .into();

    style
}

/// Install the fonts and the style on a context.
///
/// Called from `EguiManager::default` *before* the first pass, which matters:
/// `set_fonts` only takes effect from the next `begin_pass`, and painting with
/// an unbound `FontFamily::Name` panics in epaint. Installing at construction
/// means [`brand_family`] is safe to use from frame one.
///
/// The style is set for **both** themes. This integration never feeds egui a
/// `system_theme`, so it would fall back to dark on its own — but a light-theme
/// context left on egui's stock style is a latent white flash, and the app has
/// exactly one look at a time, chosen by [`set_light`] rather than by egui's
/// theme: both of egui's slots hold the palette in force.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    let style = std::sync::Arc::new(style());
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

/// Re-apply the palette in force to a context's style, after [`set_light`].
///
/// Only the visuals: the spacing, the type scale and the debug switches
/// `EguiManager::default` turns off are left as they are.
pub fn apply_visuals(ctx: &egui::Context) {
    let v = visuals();
    ctx.all_styles_mut(|style| style.visuals = v.clone());
}

// ─── The mark ─────────────────────────────────────────────────────────────────

/// The app mark, **drawn rather than sampled**: the brand pin standing on
/// Groningen's street plan, the streets cleared in a band around it so the pin
/// reads as cut out of the map.
///
/// It is a vector for the reason every other icon in this file is painted from
/// primitives: it is wanted at 84 pt on the splash and 22 pt on the sign-in
/// card, and egui has no mip chain, so a 256 px PNG sampled down to a small
/// button shimmers whenever the window moves under it. Drawing it also takes the
/// mark out of Git LFS's silent-failure path — the icon PNG is still the OS
/// window icon and the desktop entry (`systems::window_icon`), where a file is
/// what the platform wants, but nothing *inside* the app depends on that file
/// decoding any more.
///
/// **The street plan is data, not a hand-written table**: `mark_table.rs` is
/// written by `tools/brand-mark.ts` from OpenStreetMap in the same run that
/// writes `assets/icon/cartopolis.svg`, its light variant, the PNG, the .ico and
/// `index.html`'s loader copy — the web splash and this one are one screen
/// handed over mid-load (see `systems::splash`), and a mark that changes at the
/// handover is the tell that they are not. Rerun the tool; do not edit any of
/// them by hand. The one painting simplification against the icon is the tile's
/// vertical gradient, which is its middle tone here (egui has no gradient
/// primitive).
mod mark {
    use egui::Color32;

    /// Half the icon's square. Every coordinate in the table is in that square's
    /// units with `(UNIT, UNIT)` at the centre, so `paint_mark` maps them with
    /// one scale factor of `radius / UNIT`.
    pub const UNIT: f32 = 256.0;
    /// The launcher tile's corner radius.
    pub const PLATE_CORNER: f32 = 115.0;

    /// Below this radius the side streets are a grey smudge rather than a plan —
    /// a 22 pt mark gives a residential street a fifth of a pixel — so only the
    /// main roads and the canals are drawn. The pin carries the mark there.
    pub const DETAIL_RADIUS: f32 = 30.0;
    /// The narrowest line still drawn below [`DETAIL_RADIUS`].
    pub const MAJOR_WIDTH: f32 = 5.4;

    /// The tile, as one tone: the source's `#2e424e` → `#1B2830` gradient in the
    /// middle, which is what an unlit slate reads as at any size. The mark is the
    /// logo: it did not go monochrome with the UI.
    pub const PLATE: Color32 = Color32::from_rgb(0x25, 0x35, 0x3F);
    /// The street plan's two inks — cream at 50 % and 30 % over the plate,
    /// pre-mixed so crossing strokes do not double up.
    pub const STREET: Color32 = Color32::from_rgb(0x8C, 0x92, 0x95);
    pub const WATER: Color32 = Color32::from_rgb(0x62, 0x6D, 0x72);

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Ink {
        Street,
        Water,
    }

    /// One polyline of the plan.
    pub struct Line {
        pub width: f32,
        pub ink: Ink,
        pub points: &'static [[f32; 2]],
    }

    include!("mark_table.rs");
}

/// Draw [the mark](mark) centred on `center`, out to `radius` (half the tile, so
/// the splash's 84 gives a 168 pt square), at `alpha` — 1.0 opaque; the splash
/// fades the whole screen out through it.
pub fn paint_mark(p: &egui::Painter, center: egui::Pos2, radius: f32, alpha: f32) {
    // `gamma_multiply` rather than a rebuilt `from_rgba_unmultiplied`: `Color32`
    // is premultiplied, so re-wrapping its channels as straight ones darkens
    // anything that already carries an alpha.
    let a = |c: Color32| c.gamma_multiply(alpha);
    let s = radius / mark::UNIT;
    // Icon units → screen.
    let q = |v: [f32; 2]| {
        egui::pos2(
            center.x + (v[0] - mark::UNIT) * s,
            center.y + (v[1] - mark::UNIT) * s,
        )
    };
    let detail = radius >= mark::DETAIL_RADIUS;

    p.rect_filled(
        egui::Rect::from_center_size(center, egui::Vec2::splat(radius * 2.0)),
        CornerRadius::same((mark::PLATE_CORNER * s).round().clamp(0.0, 255.0) as u8),
        a(mark::PLATE),
    );

    // The plan is already cut to the tile and clear of the pin, so it is drawn
    // with no clip — egui has none to give it.
    for line in mark::LINES {
        if !detail && line.width < mark::MAJOR_WIDTH {
            continue;
        }
        let ink = match line.ink {
            mark::Ink::Street => mark::STREET,
            mark::Ink::Water => mark::WATER,
        };
        p.add(egui::Shape::line(
            line.points.iter().map(|&v| q(v)).collect(),
            Stroke::new((line.width * s).max(0.6), a(ink)),
        ));
    }

    // The teardrop is convex, so it is one polygon; its keyline is the polygon's
    // own stroke, centred on the edge as the SVG's is.
    p.add(egui::Shape::convex_polygon(
        mark::PIN_OUTLINE.iter().map(|&v| q(v)).collect(),
        a(BRAND),
        Stroke::new((mark::PIN_KEYLINE * s).max(1.0), a(MARK_CREAM)),
    ));
    p.circle_filled(q(mark::PIN_DOT_C), mark::PIN_DOT_R * s, a(MARK_CREAM));
}

/// The mark's cream: the pin's keyline and dot, `#F2EFEA` in every copy of the
/// icon. Not [`Palette::cream`], which is the chrome's text colour and a cooler white.
const MARK_CREAM: Color32 = Color32::from_rgb(0xF2, 0xEF, 0xEA);

// ─── Wordmark ─────────────────────────────────────────────────────────────────

/// The wordmark, in two tones: **cream `carto`, accent `polis`**, lower case.
///
/// Not a decoration — it is the atlas web UI's wordmark
/// (`crates/atlas/web`'s `.app-header .wordmark`, `carto<b>polis</b>`), and that
/// app talks to this simulator's admin surface. Two products that share a palette
/// and a backend should not write their own name two different ways, so the split
/// and the colours are copied from there rather than re-invented, and
/// `index.html`'s loader carries the same two spans.
pub const WORDMARK_HEAD: &str = "carto";
pub const WORDMARK_TAIL: &str = "polis";

/// The tail's orange — [`BRAND`], and no longer the accent.
///
/// **Named rather than written at each of the three sites, because they had
/// already drifted.** This was [`Palette::accent_text`] (`#FFA470`) here while
/// `index.html`'s `#wordmark b` carried `#FF9457`, so the name was painted in two
/// different colours on the two halves of one startup screen — and neither was
/// the orange beside it: the mark's ring and the loader's progress bar are plain
/// `#FD6012`. On the splash the wordmark sits directly above both, and a
/// lightened accent next to the real one reads as pink rather than as brand.
///
/// It used to be `= ACCENT`, which held while the chrome was orange and would
/// have quietly greyed the logo the day the chrome went monochrome. The
/// wordmark, the mark and the loader are the *brand*; the chrome is not.
pub const WORDMARK_TAIL_COLOR: Color32 = BRAND;

/// Paint the wordmark centred on `center` at `size` points, faded by `alpha`.
///
/// Laid out as two galleys and placed from their combined width, because
/// `Align2::CENTER_CENTER` on each piece separately would centre each of them on
/// the same point and stack the two words on top of each other.
///
/// Cream on the splash's dark in both themes ([`DARK`]): the splash is the
/// same screen as `index.html`'s loader, which is dark.
pub fn paint_wordmark(p: &egui::Painter, center: egui::Pos2, size: f32, alpha: f32) {
    let a = |c: Color32| {
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * alpha) as u8)
    };
    let font = FontId::new(size, brand_family());
    let head = p.layout_no_wrap(WORDMARK_HEAD.to_owned(), font.clone(), a(DARK.cream));
    let tail = p.layout_no_wrap(WORDMARK_TAIL.to_owned(), font, a(WORDMARK_TAIL_COLOR));
    let total = head.size().x + tail.size().x;
    let left = center.x - total * 0.5;
    let top = center.y - head.size().y * 0.5;
    let head_w = head.size().x;
    p.galley(egui::pos2(left, top), head, a(DARK.cream));
    p.galley(egui::pos2(left + head_w, top), tail, a(WORDMARK_TAIL_COLOR));
}

/// [`paint_wordmark`] for a laid-out row rather than a painter — the sign-in
/// card's brand line. One writer for the two-tone split, so the card and the
/// splash cannot drift.
pub fn wordmark_ui(ui: &mut egui::Ui, size: f32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let font = FontId::new(size, brand_family());
        ui.label(
            egui::RichText::new(WORDMARK_HEAD)
                .font(font.clone())
                .color(p().cream),
        );
        ui.label(
            egui::RichText::new(WORDMARK_TAIL)
                .font(font)
                .color(WORDMARK_TAIL_COLOR),
        );
    });
}

// ─── Glyphs ───────────────────────────────────────────────────────────────────

// The preference ladder that used to live here — `first_drawable(ctx, &["🔍",
// "⌕", "S"])`, which probed the loaded font and fell back to a letter — went
// with the icons it existed for. They come from `systems::icons` now, a font the
// app ships, so there is nothing left to fall back *to*. What the ladder was
// working around is still true of egui's own stack, and still applies to any
// non-Latin-1 character somebody types straight into a label: its coverage is
// both thinner than it looks and wider than the guesses people make about it
// (probing the four cmaps on 2026-08-10 found `⇅ ⬤ ← → ↰ ✕ ✓ ⌫ ⏎ ⇧ 🧭 🤖 🧍`
// missing and `📍 📋 ⬆ ✔ ★ 🏠 ☰ ⌖ ◎ • ⚑ ↻ ▶ ⚙ 🔍 💬 🗺` present). That is what
// `tests::no_painted_glyph_is_tofu` still walks the source for.

// ─── Shared frames ────────────────────────────────────────────────────────────

/// The frame every box that floats *over the map* shares — the status HUD, the
/// stats readout, the bottom action bar.
///
/// One function because these were three hand-rolled copies of the same
/// `Frame::NONE.fill(...)` that had already drifted apart (two alpha values, two
/// corner radii, three margins), and a "the overlays look inconsistent" bug is
/// invisible until you put two of them on screen at once.
pub fn overlay_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(p().overlay)
        .stroke(Stroke::new(1.0, p().overlay_line))
        .corner_radius(CornerRadius::same(RADIUS_LG))
        .inner_margin(Margin::symmetric(14, 12))
}

// ── On the `#[allow(dead_code)]`s in this file ────────────────────────────────
// Some of what this module exports is *vocabulary*: it exists so the call sites
// that hand-roll these today have something to convert to, and those conversions
// live in other files. The workspace denies the `unused` group, which makes an
// as-yet-uncalled item in this (private) module tree a hard error rather than a
// warning — so each carries an allow until its first caller arrives. **Delete
// the attribute with the first conversion**, or the next genuinely dead thing
// added here goes unnoticed.

/// Padding inside a label chip. Exported beside [`chip_frame`] because half the
/// chips in this app are painted rather than laid out — a pin label is a galley
/// and a `rect_filled` under it, which no `Frame` can reach — and those sites
/// need the same two numbers.
pub const CHIP_PADDING: egui::Vec2 = egui::vec2(6.0, 3.0);

/// The frame around a small text chip painted over the map: a place-pin label,
/// the bookmark drag ghost, a feature's hover card.
///
/// Sibling of [`overlay_frame`], and here for the same reason: three files
/// hand-rolled the box around [`LABEL_CHIP`] with three paddings and three
/// radii, and two of them appear on the map *at once* (a pin label and the ghost
/// being dragged over it), where "the chips look inconsistent" is not a subtle
/// difference — it is two designs in one glance.
///
/// A chip is not an [`overlay_frame`]: it carries a few words over an arbitrary
/// tile rather than a box of controls, so it is darker, tighter, and rounded with
/// [`RADIUS_SM`] because at ~20 pt tall [`RADIUS`] would make it a lozenge. Add a
/// `.stroke(...)` for the ones that carry a folder's colour.
pub fn chip_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(LABEL_CHIP)
        .corner_radius(CornerRadius::same(RADIUS_SM))
        .inner_margin(Margin::symmetric(
            CHIP_PADDING.x as i8,
            CHIP_PADDING.y as i8,
        ))
}

/// Height of a primary button on a pointer. Under a finger it grows to
/// [`row_height`], which is larger.
pub const PRIMARY_BUTTON_HEIGHT: f32 = 40.0;

/// **The** call to action on a screen: filled with the accent, labelled in
/// [`Palette::ink`] at [`SIZE_VALUE`], taller than an ordinary button, and at least
/// `min_width` points wide.
///
/// There should be at most one per screen, and it should be the thing you came to
/// do — Sign in, Start driving. Everything else is `ui.button`.
///
/// Exists because the app already had two primary buttons that did not look
/// alike: the sign-in CTA was a hand-built filled `Button` and the Route panel's
/// equally-primary "▶ Driver mode" was a plain `ui.button`, so the same rank read
/// as two different ones. `INK` on `ACCENT` and not white is the palette's rule for
/// text on the accent (8.3:1 against 2.2:1) and is unit-tested below.
///
/// The width is always stated by the caller. A no-width `primary_button` wrapper
/// over a `PRIMARY_BUTTON_MIN_WIDTH` constant shipped beside this and never grew
/// a caller — both CTAs in the app fill the card they sit in — so it went rather
/// than sit behind an `allow(dead_code)` hiding the next genuinely dead thing.
///
/// **The label is centred by two growable atoms, not by the surrounding layout.**
/// egui 0.35 takes a button's atom alignment from `ui.layout()` — and a top-down
/// `Ui`, which is what every card, panel and dialog in this app hands down, is
/// `Align::Min`. So a button whose `min_size` is wider than its text put that
/// text against the left edge: the sign-in card's full-width CTA read as an
/// orange bar with a word stuck in the corner. An empty [`egui::Atom::grow`]
/// either side is the fix that does not touch where the *button* itself sits,
/// which the alternative — wrapping it in a `Align::Center` layout — would,
/// centring a narrow button in its row as a side effect.
///
/// **And it answers the pointer.** The fill was a flat [`Palette::accent`] in every
/// state, so the one control on the screen whose whole job is to be pressed gave
/// nothing back when you pressed it — no hover, no press, no focus ring, on a
/// button that then waits on a network round trip. It lifts to
/// [`Palette::accent_bright`] under the pointer, sinks to [`Palette::accent_pressed`] while held,
/// and wears an [`Palette::accent_ring`] outline when the keyboard is on it. The state
/// comes from the *previous* pass's response, read under the id this button is
/// about to take — the same thing egui's own `Button` does internally, and the
/// reason `next_auto_id` is safe here is that it reports the next id without
/// consuming it.
pub fn primary_button_sized(ui: &mut egui::Ui, label: &str, min_width: f32) -> egui::Response {
    let height = PRIMARY_BUTTON_HEIGHT.max(row_height());
    let was = ui.ctx().read_response(ui.next_auto_id());
    let held = was
        .as_ref()
        .is_some_and(egui::Response::is_pointer_button_down_on);
    let hovered = was.as_ref().is_some_and(egui::Response::hovered);
    let focused = was.as_ref().is_some_and(egui::Response::has_focus);
    let fill = if !ui.is_enabled() {
        p().accent_muted
    } else if held {
        p().accent_pressed
    } else if hovered {
        p().accent_lift
    } else {
        p().accent
    };
    // Depth (see `lift_shadow`): the accent's glow under it and a lit top edge,
    // both gone while it is held — a pressed key is down in the glass. The glow
    // has to go *under* the button, so its slot is reserved before the button
    // paints and filled once the rect is known.
    let lit = ui.is_enabled() && !held;
    let glow_slot = ui.painter().add(egui::Shape::Noop);
    let resp = ui.add(
        egui::Button::new((
            egui::Atom::grow(),
            egui::RichText::new(label)
                .size(SIZE_VALUE)
                .strong()
                .color(p().on_accent),
            egui::Atom::grow(),
        ))
        .fill(fill)
        .stroke(if focused {
            Stroke::new(FOCUS_RING, p().accent_ring)
        } else {
            Stroke::NONE
        })
        .corner_radius(CornerRadius::same(RADIUS))
        .min_size(egui::vec2(min_width, height)),
    );
    // Flat, as the midas hub's primary: the small shadow every button wears,
    // no glow and no lit edge.
    if lit {
        ui.painter().set(
            glow_slot,
            segment_shadow().as_shape(resp.rect, CornerRadius::same(RADIUS)),
        );
    }
    resp
}

/// The button beside a [`primary_button_sized`]: as tall, on a lit surface
/// with a hairline, the icon in the accent and the word in [`Palette::cream`].
///
/// It replaces two things that each read wrong in a card: an
/// [`action_button`], which is an outline pill the size of a line of text and
/// so too small to aim at, and a [`round_button`], whose fill is the map's
/// translucent overlay and is nearly black on a panel. An empty `label` makes
/// it an icon-only square when `min_width` is the height.
pub fn secondary_button(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    min_width: f32,
) -> egui::Response {
    secondary_toggle(ui, icon, label, min_width, false)
}

/// [`secondary_button`] for a switch: `on` wears the accent's tint and ring.
pub fn secondary_toggle(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    min_width: f32,
    on: bool,
) -> egui::Response {
    // Painted rather than an `egui::Button`: a button lays its atoms out with
    // padding and gaps of its own, so an icon-only one came out wider than the
    // square asked for, and a row of them pushed the panel wider every frame.
    // Here the size asked for is the size drawn.
    let height = PRIMARY_BUTTON_HEIGHT.max(row_height());
    let gap = 6.0;
    let mut icon_g = (!icon.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(icon.to_string(), FontId::proportional(17.0), p().accent)
    });
    let word_g = (!label.is_empty()).then(|| {
        ui.painter().layout_no_wrap(
            label.to_string(),
            FontId::new(SIZE_BODY, brand_family()),
            p().cream,
        )
    });
    let content = |icon: &Option<std::sync::Arc<egui::Galley>>| {
        let i = icon.as_ref().map_or(0.0, |g| g.size().x);
        let w = word_g.as_ref().map_or(0.0, |g| g.size().x);
        i + w + if i > 0.0 && w > 0.0 { gap } else { 0.0 }
    };
    // A button sharing a row (`equal_width`) gives up room before it widens
    // past its share: the padding first, then the icon, keeping the word. A
    // row that widened instead pushed the whole panel past a phone's edge —
    // the community page's People · Topic · 5 · Edit took 290 pt of a 250 pt
    // panel, and every card below it was cut off on the right (2026-10-31).
    let mut pad = 12.0;
    if word_g.is_some() && min_width > 0.0 && content(&icon_g) + 2.0 * pad > min_width {
        pad = 6.0;
        if content(&icon_g) + 2.0 * pad > min_width {
            icon_g = None;
        }
    }
    let content = content(&icon_g);
    let width = if word_g.is_some() {
        min_width.max(content + 2.0 * pad)
    } else {
        min_width.max(height)
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    // An icon-only one (`label` empty) is named again by its caller, from its
    // tooltip: a later `widget_info` replaces this one.
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, label)
    });
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let enabled = ui.is_enabled();
    let held = resp.is_pointer_button_down_on();
    // The midas hub's button (`m-button`): a dark surface with a hairline and
    // a small shadow, the word and its icon in the strong text tone. Under the
    // pointer it lifts a step and the hairline takes the accent; "on" is the
    // hub's soft teal tag. The hairline is [`Palette::control_edge`], the hub's
    // soft border a step firmer.
    let (fill, line, ink) = if !enabled {
        (p().surface, p().surface_hover, p().cream)
    } else if on {
        (p().accent_muted, p().accent_muted, p().accent_word)
    } else if held {
        (p().surface_hover, p().line, p().cream)
    } else if resp.hovered() {
        (p().surface_raised, p().accent, p().cream)
    } else {
        (p().surface, p().control_edge, p().cream)
    };
    let p = ui.painter();
    if enabled && !held && !on {
        p.add(segment_shadow().as_shape(rect, CornerRadius::same(RADIUS)));
    }
    p.rect(
        rect,
        CornerRadius::same(RADIUS),
        fill,
        Stroke::new(1.0, line),
        egui::StrokeKind::Inside,
    );
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.5) };
    let mut x = rect.center().x - content / 2.0;
    if let Some(g) = icon_g {
        let w = g.size().x;
        p.galley_with_override_text_color(
            egui::pos2(x, rect.center().y - g.size().y / 2.0),
            g,
            dim(ink),
        );
        x += w + gap;
    }
    if let Some(g) = word_g {
        let y = rect.center().y - g.size().y / 2.0;
        p.galley_with_override_text_color(egui::pos2(x, y), g, dim(ink));
    }
    paint_focus_ring(ui, &resp, rect, false);
    resp
}

/// The width each of `n` buttons gets to share a row equally: a row of
/// controls that are one choice each reads as a set, sized to their words it
/// reads as a pile.
pub fn equal_width(ui: &egui::Ui, n: usize) -> f32 {
    let n = n.max(1) as f32;
    ((ui.available_width() - ui.spacing().item_spacing.x * (n - 1.0)) / n).floor()
}

/// Diameter of a round icon button, in points. Matches the compass disc in the
/// opposite corner — the map's floating controls are one family. Already 60 dp on
/// the touch tier, so it needs no finger-sized variant.
pub const ROUND_BUTTON_SIZE: f32 = 40.0;

/// Width of the ring that says "the keyboard is here".
const FOCUS_RING: f32 = 2.0;

/// Draw the keyboard-focus ring *outside* `rect`, if this widget has focus.
///
/// The four controls in this file that are painted rather than assembled from
/// `Button` all branched on `hovered()` and `is_pointer_button_down_on()` only —
/// and they allocate with `Sense::click()`, which is focusable, so Tab lands on
/// them and nothing shows it. A keyboard user tabbing the dock had no way to
/// know where they were.
///
/// **Two rings, dark under light**, because this one is drawn *outside* the
/// control and therefore lands on whatever is behind it — which for the dock and
/// the compass is the map, at any brightness the day and the imagery can produce.
/// A single [`Palette::cream`] ring is 16.7:1 over the panel and invisible over white
/// paper; a single [`Palette::ink`] one is the reverse. The pair cannot both disappear, so
/// the outline is legible everywhere without the palette having to guess what is
/// under it. It used to be the light ring alone, and the monochrome chrome makes
/// that worse rather than better: there is no longer a saturated colour to fall
/// back on.
///
/// [`Palette::cream`] and not the accent for the inner ring: an accent ring is the
/// *pressed* state, and would say the wrong thing.
fn paint_focus_ring(ui: &egui::Ui, resp: &egui::Response, rect: egui::Rect, round: bool) {
    if !resp.has_focus() {
        return;
    }
    let p = ui.painter();
    let ring = rect.expand(FOCUS_RING);
    // Outer first, wider, so the light ring is drawn over the middle of it and
    // what is left either side is a dark edge on both flanks.
    for (width, colour) in [
        (FOCUS_RING * 2.0, self::p().ink),
        (FOCUS_RING, self::p().cream),
    ] {
        let stroke = Stroke::new(width, colour);
        if round {
            p.circle_stroke(ring.center(), ring.width() * 0.5, stroke);
        } else {
            p.rect_stroke(
                ring,
                CornerRadius::same(RADIUS_SM),
                stroke,
                egui::StrokeKind::Outside,
            );
        }
    }
}

/// A round icon button that floats over the map: the dock along the bottom, and
/// anything else that wants the compass's visual language.
///
/// Painted rather than assembled from `Button`, because egui's button is a
/// rounded *rect* whose corner radius is capped well below half its height — a
/// "circular" button built that way is a squircle that reads as a mistake next to
/// the genuinely round compass. `selected` is the open-panel state and wears the
/// engaged plate — [`Palette::accent`] with [`Palette::ink`] on it, the same inversion every "this
/// is on" control in the app uses.
pub fn round_button(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    selected: bool,
) -> egui::Response {
    round_button_on(ui, icon, tooltip, selected, (p().overlay, p().overlay_line))
}

/// One button of the dock: a tall rounded plate with its glyph and its word
/// inside, [`ROUND_BUTTON_SIZE`] wide and [`DOCK_BUTTON_HEIGHT`] tall.
///
/// It was a round disc with the word painted under it on the map in a dark
/// halo; over a busy city the halo smudged the word, worst in the light theme
/// ("the text under the floating buttons", 2026-11-10). Inside the plate the
/// word is on the panel's own tone, so it reads the same over any map.
pub fn dock_button(
    ui: &mut egui::Ui,
    icon: &str,
    word: &str,
    tooltip: &str,
    selected: bool,
) -> egui::Response {
    let size = egui::vec2(ROUND_BUTTON_SIZE, DOCK_BUTTON_HEIGHT);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = named(ui, resp, tooltip);
    let (fill, rim, ink, word_ink) = match (selected, resp.hovered()) {
        (true, _) => (p().accent, p().accent_ring, p().on_accent, p().on_accent),
        (false, true) => (p().surface_hover, p().overlay_line, p().cream, p().cream),
        (false, false) => (p().overlay, p().overlay_line, p().text, p().text_dim),
    };
    let corner = CornerRadius::same(14);
    let painter = ui.painter();
    painter.add(lift_shadow().as_shape(rect, corner));
    painter.rect(
        rect,
        corner,
        fill,
        Stroke::new(1.0, rim),
        egui::StrokeKind::Inside,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.top() + 18.0),
        egui::Align2::CENTER_CENTER,
        icon,
        FontId::proportional(17.0),
        ink,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 11.0),
        egui::Align2::CENTER_CENTER,
        word,
        FontId::proportional(SIZE_TINY),
        word_ink,
    );
    paint_focus_ring(ui, &resp, rect, false);
    resp.on_hover_text(tooltip)
}

/// Height of a [`dock_button`]: the glyph's disc and its word, in one plate.
pub const DOCK_BUTTON_HEIGHT: f32 = 50.0;

fn round_button_on(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    selected: bool,
    rest: (Color32, Color32),
) -> egui::Response {
    let size = egui::Vec2::splat(ROUND_BUTTON_SIZE);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = named(ui, resp, tooltip);
    let hovered = resp.hovered();
    let (fill, ring, ink) = match (selected, hovered) {
        (true, _) => (p().accent, p().accent_ring, p().on_accent),
        (false, true) => (p().surface_hover, p().accent, p().cream),
        (false, false) => (rest.0, rest.1, p().text),
    };
    paint_disc(
        ui.painter(),
        rect.center(),
        ROUND_BUTTON_SIZE * 0.5 - 1.0,
        Disc {
            fill,
            ring,
            ink,
            icon,
            icon_size: 17.0,
            ring_width: 1.5,
        },
    );
    paint_focus_ring(ui, &resp, rect, true);
    resp.on_hover_text(tooltip)
}

/// Give a painted control its name, as egui's own `Button` gives its text.
///
/// A control allocated with `allocate_exact_size` and painted by hand is,
/// to egui, a clickable rectangle with no name: screen readers and the MCP
/// `ui_tree` (`mcp::screen`) saw the dock and the map controls as sixteen
/// unlabelled areas. The name goes into egui's widget info, nowhere visible.
pub fn named(ui: &egui::Ui, resp: egui::Response, label: &str) -> egui::Response {
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    resp
}

/// Hang-up red: the one control in the app that is red at rest, because
/// ending a call is the action every phone draws in it, and a person reaching
/// for it in a hurry looks for the colour before the shape.
pub const CALL_END_RED: Color32 = Color32::from_rgb(0xD6, 0x45, 0x45);

/// Answer green: the other half of [`CALL_END_RED`], on the one screen where
/// both appear — a call coming in.
pub const CALL_ACCEPT_GREEN: Color32 = Color32::from_rgb(0x2E, 0xA0, 0x5A);

/// What a [`call_button`] is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CallTone {
    /// A switch that is on (the microphone, the camera): a frosted plate that
    /// reads over a picture as well as over the dark call screen.
    On,
    /// A switch that is *off* wears the solid cream plate: that is the state
    /// worth noticing, and the icon says which (`MIC_OFF`, `VIDEOCAM_OFF`).
    Off,
    /// Hang up, or decline: [`CALL_END_RED`].
    HangUp,
    /// Answer: [`CALL_ACCEPT_GREEN`].
    Accept,
}

/// The call screen's big buttons: a thumb's width, as on a phone's call screen.
pub const CALL_BUTTON_LARGE: f32 = 64.0;
/// The bar a minimised call or a ringing banner shows.
pub const CALL_BUTTON_SMALL: f32 = 36.0;

/// A round call control, `diameter` across.
pub fn call_button(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    tone: CallTone,
    diameter: f32,
) -> egui::Response {
    let size = egui::Vec2::splat(diameter);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = named(ui, resp, tooltip);
    let hovered = resp.hovered();
    let pressed = resp.is_pointer_button_down_on();
    let solid = |c: Color32| {
        let c = if pressed {
            c.gamma_multiply(0.85)
        } else if hovered {
            c.gamma_multiply(1.15)
        } else {
            c
        };
        // The glyph on a call colour is white in both themes, as on a phone.
        (c, c, DARK.cream)
    };
    // The frosted plate of a switch that is on: white over the dark call
    // screen, black over a light banner.
    let veil = |a: u8| {
        if p().dark {
            Color32::from_white_alpha(a)
        } else {
            Color32::from_black_alpha(a / 2)
        }
    };
    let (fill, ring, ink) = match tone {
        CallTone::HangUp => solid(CALL_END_RED),
        CallTone::Accept => solid(CALL_ACCEPT_GREEN),
        CallTone::Off => (
            if hovered { p().accent_ring } else { p().cream },
            p().accent_ring,
            p().ink,
        ),
        CallTone::On => (
            veil(if pressed {
                70
            } else if hovered {
                56
            } else {
                38
            }),
            veil(46),
            p().cream,
        ),
    };
    paint_disc(
        ui.painter(),
        rect.center(),
        diameter * 0.5 - 1.0,
        Disc {
            fill,
            ring,
            ink,
            icon,
            icon_size: (diameter * 0.44).round(),
            ring_width: 1.0,
        },
    );
    paint_focus_ring(ui, &resp, rect, true);
    resp.on_hover_text(tooltip)
}

// The dock's menu disc used to be `mark_button` — [the mark](paint_mark) at
// [`round_button`]'s size, so the app's own logo was the button that opened the
// menu. It is an ordinary `round_button` carrying `icons::MENU` now: in a row
// where every other disc is a picture of what it does, the brand was the one
// that said nothing about its own contents. `paint_mark` keeps its other two
// callers (the boot splash and the sign-in card), which is where a logo belongs.

// ─── The map's floating controls ──────────────────────────────────────────────
//
// Everything that floats over the map — the dock, the compass column, the
// on-screen navigation controls, the scale bar — is laid out from these three
// numbers and painted by [`map_disc`]. They used to be per-module constants, and
// what that bought was six edge insets (8, 10, 12, 14, 16 and a bare `-10`),
// four disc diameters (28, 40, 44, 46) and two different answers to what a held
// control looks like, all within one screen. The point of a floating control is
// that it reads as *chrome over a map* rather than as part of the map, and that
// only works if there is one of it.

/// Inset of a floating control from the window edge, in points. One number for
/// all four edges: a top-left column indented 12 and a top margin of 8 is a
/// corner that looks like a mistake.
pub const OSD_MARGIN: f32 = 12.0;
/// Gap between two controls that belong to one cluster (the two halves of a
/// zoom pair, the compass and the locate button under it).
pub const OSD_GAP: f32 = 8.0;
/// Gap between two clusters sharing an edge. Deliberately more than twice
/// [`OSD_GAP`]: with one gap for both, a column of discs reads as a single
/// undifferentiated strip, which is what the layers disc and the compass under
/// it had become.
pub const OSD_GROUP_GAP: f32 = 18.0;

/// Where a [`map_disc`] wants its mark drawn, and in what ink.
pub struct DiscFace {
    pub center: egui::Pos2,
    pub radius: f32,
    pub ink: Color32,
}

/// Allocate and paint one floating map disc, and hand back where to draw its
/// mark. The plate — fill, ring, hover and pressed states, focus ring — is this
/// function's business; the glyph inside it is the caller's, because half of
/// them are painted from line segments rather than typed ([`close_button`] has
/// the reason).
///
/// `on` is the engaged state — a panel open, a lock held, a button pressed — and
/// wears the same [`Palette::accent`] plate a selected dock disc does, so "this is on" is
/// one treatment everywhere on the map: a dark glyph on a teal plate, which
/// is unmistakable against a map at any brightness. `tone` is the rim that engagement carries: [`Palette::accent_ring`]
/// normally, [`Palette::accent_bright`] for the *held* states (north-up lock,
/// follow-and-rotate), which also draw a second, heavier ring — with no second
/// accent left, "held" is said by weight, not by colour.
///
/// `sense` is the caller's, because the tests differ by what the control means:
/// a rate wants `click_and_drag` (a finger drifting a point while held is a
/// drag, and `clicked()` drops the whole push), an edge wants `click()`.
pub fn map_disc(
    ui: &mut egui::Ui,
    size: f32,
    on: bool,
    tone: Color32,
    sense: egui::Sense,
) -> (egui::Response, DiscFace) {
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(size), sense);
    let held = resp.is_pointer_button_down_on();
    let (fill, ring, ink) = match (on || held, resp.hovered()) {
        (true, _) => (p().accent, tone, p().on_accent),
        (false, true) => (p().surface_hover, p().accent, p().cream),
        (false, false) => (p().overlay, p().overlay_line, p().text),
    };
    let radius = size * 0.5 - 1.0;
    let p = ui.painter();
    paint_disc_shadow(p, rect.center(), radius, lift_shadow());
    p.circle_filled(rect.center(), radius, fill);
    p.circle_stroke(rect.center(), radius, Stroke::new(1.5, ring));
    paint_contact_edge(p, rect.center(), radius + 0.75);
    paint_focus_ring(ui, &resp, rect, true);
    (
        resp,
        DiscFace {
            center: rect.center(),
            radius,
            ink,
        },
    )
}

/// Midas's gold: the hub's theme and its icon (midas `web/src/icon.svg`), which
/// is [`Palette::warn`]'s value because both are that theme's warning colour —
/// in the light theme too, where the hub's warning is an amber-brown that
/// reads on the light disc the mark sits on.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // the link is native-only
#[must_use]
pub fn midas_gold() -> Color32 {
    p().warn
}

/// The Midas mark — an M of five joined points, the hub and its machines — in
/// [`midas_gold`], drawn on a disc of `radius` at `c`. Its geometry is the
/// icon's, in that SVG's 64-unit square centred on (32, 32); the gold square
/// behind it there is the disc here.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub fn paint_midas_mark(p: &egui::Painter, c: egui::Pos2, radius: f32) {
    let gold = midas_gold();
    let s = radius / 26.0;
    let at = |x: f32, y: f32| c + egui::vec2(x - 32.0, y - 32.0) * s;
    let points = [
        at(20.0, 43.0),
        at(20.0, 21.0),
        at(32.0, 33.0),
        at(44.0, 21.0),
        at(44.0, 43.0),
    ];
    p.add(egui::Shape::line(
        points.to_vec(),
        Stroke::new(5.0 * s, gold),
    ));
    for pt in points {
        p.circle_filled(pt, 4.0 * s, gold);
    }
}

/// The look of one icon disc, shared by [`round_button`] and the medallion a
/// panel header wears — they have to be the same object at two sizes, or the
/// header stops reading as "the dock button you just pressed".
struct Disc<'a> {
    fill: Color32,
    ring: Color32,
    ink: Color32,
    icon: &'a str,
    icon_size: f32,
    /// Width of the ring. A disc that floats over the map needs the heavier one
    /// ([`map_disc`] uses the same value): its ring is the only thing separating
    /// it from a bright tile, and at 1 pt over pale beige paper the plate read as
    /// a flat hole with no edge at all. A medallion inside a panel header has the
    /// header band behind it and keeps the hairline.
    ring_width: f32,
}

/// A translucent-black hairline just outside a floating plate.
///
/// **The cost of inverting the engaged state, and the fix for it.** An engaged
/// disc is [`Palette::accent`] now — a light plate — and the map's brightest regions are
/// the sky and the near-white paper the basemap is printed on, so over those the
/// plate and its lighter rim both dissolve and the control disappears at exactly
/// the moment it is *on*. That is the same failure the *off* state used to have
/// over bright tiles, which is why its ring went to 1.5 pt; the light plate needs
/// the other half of the answer.
///
/// Black at low alpha rather than [`Palette::ink`], for the reason [`Palette::overlay_line`] is
/// white at low alpha: it composites, so it is a soft contact shadow on anything
/// pale and simply not there on anything dark — where the light rim is already
/// doing the work. Both edges are always drawn and neither has a state to get
/// wrong.
fn paint_contact_edge(p: &egui::Painter, center: egui::Pos2, radius: f32) {
    p.circle_stroke(
        center,
        radius,
        Stroke::new(1.0, Color32::from_black_alpha(110)),
    );
}

// ─── Depth ────────────────────────────────────────────────────────────────────
//
// One light, from above — the same scene the web UI draws in `depth.css`. A
// control standing on the map casts a soft shadow under it. (It also caught the
// light along its top, and an engaged one glowed, until the glass went
// on 2026-11-08; a flat plate under one shadow is the midas hub's look.)

/// The shadow a floating control casts: a little below it and soft, so it
/// reads as standing off the surface rather than printed on it.
fn lift_shadow() -> Shadow {
    shadow([0, 3], 12, 100)
}

/// A soft black shadow at `alpha` in the dark theme, scaled by
/// [`Palette::shadow`]: on a light page the hub's lift is a faint one.
fn shadow(offset: [i8; 2], blur: u8, alpha: u8) -> Shadow {
    Shadow {
        offset,
        blur,
        spread: 0,
        color: Color32::from_black_alpha((f32::from(alpha) * p().shadow).round() as u8),
    }
}

/// The shadow under a card in a panel ([`list_group`], a chat row, a community
/// row): the midas hub's `--lift-2`, as near as one egui shadow comes — a few
/// points down, soft, darker than the page, so a card on a panel reads as
/// lying on it rather than printed into it.
#[must_use]
pub fn card_shadow() -> Shadow {
    shadow([0, 4], 14, 150)
}

/// The shadow under the chosen option of a [`segmented`] control and under a
/// button at rest: the hub's `--m-shadow`, close and small.
fn segment_shadow() -> Shadow {
    shadow([0, 1], 4, 120)
}

/// The chosen option of a segmented control, painted: a raised card with the
/// small shadow ([`segmented`], the Community | World switch).
pub fn paint_raised(painter: &egui::Painter, rect: egui::Rect, corner: CornerRadius) {
    painter.add(segment_shadow().as_shape(rect, corner));
    painter.rect_filled(rect, corner, p().surface_raised);
}

/// A card in a panel, painted: its [`card_shadow`] and then its fill. For the
/// rows that are painted rather than laid out in a `Frame` (a `Frame` takes
/// `.shadow(card_shadow())`).
pub fn paint_card(p: &egui::Painter, rect: egui::Rect, corner: CornerRadius, fill: Color32) {
    p.add(card_shadow().as_shape(rect, corner));
    p.rect_filled(rect, corner, fill);
}

/// A shadow (or glow) under a disc. Painted before the disc itself.
fn paint_disc_shadow(p: &egui::Painter, center: egui::Pos2, radius: f32, shadow: Shadow) {
    let rect = egui::Rect::from_center_size(center, egui::Vec2::splat(radius * 2.0));
    let corner = CornerRadius::same(radius.min(f32::from(u8::MAX)) as u8);
    p.add(shadow.as_shape(rect, corner));
}

/// The soft shadow a round button stands on, for a disc painted outside this
/// file (the layer categories), so it lifts as the map's round buttons do.
pub fn paint_disc_lift(p: &egui::Painter, center: egui::Pos2, radius: f32) {
    paint_disc_shadow(p, center, radius, lift_shadow());
}

fn paint_disc(p: &egui::Painter, center: egui::Pos2, radius: f32, d: Disc<'_>) {
    // A flat plate under one soft shadow, as the midas hub draws its round
    // buttons. The sheen and the accent's glow went with the glass (2026-11-08).
    paint_disc_shadow(p, center, radius, lift_shadow());
    p.circle_filled(center, radius, d.fill);
    paint_contact_edge(p, center, radius + d.ring_width * 0.5);
    p.circle_stroke(center, radius, Stroke::new(d.ring_width, d.ring));
    p.text(
        center,
        egui::Align2::CENTER_CENTER,
        d.icon,
        FontId::proportional(d.icon_size),
        d.ink,
    );
}

// ─── Panel header ─────────────────────────────────────────────────────────────

/// Padding between the header band's edge and its contents.
const HEADER_INSET: f32 = 7.0;

/// Side of the header's close button, in points.
///
/// 22 pt is ~33 dp, which is the smallest target in the app and attached to the
/// most irreversible-looking action in a panel; under a finger it takes
/// [`row_height`] instead.
#[must_use]
pub fn close_button_size() -> f32 {
    if touch_layout() {
        row_height()
    } else {
        22.0
    }
}

/// Height of the band [`panel_header`] paints, in points — whatever the close
/// button needs plus its inset, floored at the pointer tier's 38.
fn header_height() -> f32 {
    44.0_f32.max(close_button_size() + 2.0 * HEADER_INSET)
}

/// Should an icon action beside a field be drawn `small()`?
///
/// The answer is "only for a pointer", and the reason is that
/// `egui::Button::small()` is not merely a smaller button: it **zeroes the
/// vertical padding** and drops to the `Small` text style, so the widget's
/// height collapses to the glyph's. On the touch tier that is a ~21 dp target
/// against a 48 dp guideline — a third of the area of the row it sits in, on
/// controls that are anything but incidental (pick this point on the map, use my
/// position, clear the field, rename this place).
///
/// Split out as a predicate so the policy is testable without a `Context`.
#[must_use]
pub fn actions_are_small() -> bool {
    !touch_layout()
}

/// Horizontal padding for a finger-sized icon action, against the style's own
/// 11 pt.
///
/// **The height is the guideline; the width is a negotiation.** A bookmark row
/// carries four of these (route here, update, rename, delete) beside a name that
/// has to stay readable, and at full padding they take two-thirds of a phone's
/// panel. A fingertip contact patch is taller than it is wide, so trading width
/// for the row's *height* is the same bargain `onscreen_keyboard` makes when ten
/// key columns will not fit in 393 dp — and it is the one that keeps the target
/// hittable while leaving the row legible.
const ACTION_PAD_X: f32 = 6.0;

/// An icon action that sits *beside* something — the route panel's pick /
/// use-my-position / clear, the address bar's flag, a bookmark's rename and
/// delete. Compact under a mouse, finger-sized under a finger.
pub fn action_button(ui: &mut egui::Ui, label: impl Into<egui::WidgetText>) -> egui::Response {
    action_enabled(ui, true, label)
}

/// [`action_button`], for the ones that are conditionally live — "use my
/// position" without a fix, "update to here" on a place already at the camera.
pub fn action_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    if actions_are_small() {
        return ui.add_enabled(enabled, egui::Button::new(label).small());
    }
    // A scope so the padding change cannot leak into the rest of the row; it
    // inherits the enclosing layout, so a single widget inside it lands exactly
    // where the widget alone would have.
    ui.scope(|ui| {
        ui.spacing_mut().button_padding.x = ACTION_PAD_X;
        ui.add_enabled(enabled, egui::Button::new(label))
    })
    .inner
}

/// The width of a menu entry's icon column. Fixed, so the words line up down the
/// card whatever the glyphs are — a row of labels each starting where its own
/// icon happened to end is the thing that makes a list read as a pile.
const MENU_ICON_COL: f32 = 26.0;

/// The height of one [`menu_entry`] row: a list row in the midas hub, not a
/// widget row. 36 pt on a pointer and 48 under a finger, since 2026-11-08 —
/// they were [`row_height`], 24 and 32, and "the menu items are too cramped"
/// was about these first.
#[must_use]
pub fn menu_row_height() -> f32 {
    if touch_layout() {
        48.0
    } else {
        36.0
    }
}

/// One line of the app menu: an icon, a word, and **no box**.
///
/// The menu was a stack of `ui.button`s in a justified layout, which egui draws
/// as full-width plates — a dozen of them, each looking like the primary action
/// of a dialog, with the four `selectable_label` entries between them drawing
/// nothing at all. So half the card looked like a form of buttons and half like
/// a list, and neither half said which entries were *on*.
///
/// A menu row is not a button: it is a line of a list you point at. It carries
/// its highlight on hover and its state in the accent, and that is the whole
/// visual vocabulary — the same one [`close_button`] uses for a frameless
/// control, one rung quieter than [`chip`].
///
/// Painted rather than assembled from `SelectableLabel` for the reason
/// [`round_button`] is painted: the icon column has to be a *column*, and a
/// label containing a glyph and two spaces is not one.
pub fn menu_entry(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    menu_entry_full(ui, icon, label, false, true)
}

/// [`menu_entry`] for a row that shows whether the thing it opens is open — the
/// panels, whose discs in the dock carry the same "this is the live one" accent.
pub fn menu_entry_selected(
    ui: &mut egui::Ui,
    selected: bool,
    icon: &str,
    label: &str,
) -> egui::Response {
    menu_entry_full(ui, icon, label, selected, true)
}

/// [`menu_entry`] for a row that is conditionally live — Home with no Home set,
/// Back at the start of the history.
pub fn menu_entry_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    icon: &str,
    label: &str,
) -> egui::Response {
    menu_entry_full(ui, icon, label, false, enabled)
}

fn menu_entry_full(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    // Full width whatever the layout: these live inside a fixed-width card, and a
    // row that sized to its own text would put the hover highlight on the words
    // rather than on the line.
    let height = menu_row_height().max(ui.spacing().interact_size.y);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, selected, label)
    });
    let hovered = enabled && resp.hovered();
    let ink = match (enabled, selected, hovered) {
        (false, _, _) => p().text_dim.gamma_multiply(0.6),
        (_, true, _) => p().accent_text,
        (_, false, true) => p().cream,
        _ => p().text,
    };
    let p = ui.painter();
    if hovered || resp.is_pointer_button_down_on() {
        let fill = if resp.is_pointer_button_down_on() {
            self::p().accent_muted
        } else {
            self::p().surface_hover
        };
        p.rect_filled(rect, CornerRadius::same(RADIUS_SM), fill);
    }
    if selected {
        // The same inset accent bar the sign-in card's rows wear: it is what says
        // "this one" without a box round it.
        let bar = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(1.0, 5.0),
            egui::vec2(3.0, height - 10.0),
        );
        p.rect_filled(bar, CornerRadius::same(2), self::p().accent);
    }
    let x = rect.left() + 8.0;
    if !icon.is_empty() {
        p.text(
            egui::pos2(x + MENU_ICON_COL * 0.5, rect.center().y),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(SIZE_VALUE),
            ink,
        );
    }
    p.text(
        egui::pos2(x + MENU_ICON_COL + 4.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(SIZE_VALUE),
        ink,
    );
    paint_focus_ring(ui, &resp, rect, false);
    resp
}

/// One segment of a switcher — `Auto`/`Day`/`Night`, `Auto`/`Manual`, the
/// clock's `Local`/`UTC`.
///
/// **A chip has to look like a control even when it is off.** These were
/// `ui.selectable_label`, whose unselected state paints no background and no
/// outline at all: the sun switcher rendered as the bare words "Auto Day Night"
/// with one of them in a coloured pill, so two of the three read as a caption for
/// the third rather than as the two other choices. Off is a plate now — the same
/// raised surface and hairline every button at rest wears — and on is the light
/// engaged plate the dock discs and [`map_disc`] wear for "this is the live one",
/// which is also what a segmented control looks like everywhere else: the
/// selected segment is the teal one.
///
/// It takes its height from the enclosing `interact_size` rather than pinning
/// one, which is what keeps it finger-sized inside a box that has already raised
/// that floor for the touch tier without making it a 32 pt slab on a desktop.
pub fn chip(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    ui.add(chip_button(label, selected))
}

/// [`chip`] at a stated size — for a segmented row whose segments have to be
/// equal width whatever their words are (the route sheet's travel modes).
///
/// Three chips sized to "Car", "Bike" and "Walk" read as three unrelated
/// buttons; one control with three positions has one width per position.
pub fn chip_sized(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    size: egui::Vec2,
) -> egui::Response {
    ui.add_sized(size, chip_button(label, selected))
}

fn chip_button(label: &str, selected: bool) -> egui::Button<'static> {
    let (fill, stroke, ink) = if selected {
        (p().accent, p().accent_ring, p().on_accent)
    } else {
        (p().surface_raised, p().control_edge, p().text)
    };
    egui::Button::new(egui::RichText::new(label).size(SIZE_CHIP).color(ink))
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(RADIUS_SM))
}

/// The ✕ every panel closes with — **painted, not typed.**
///
/// `✕` is in none of the fonts egui ships, so every panel that
/// spelled its close button `ui.button("✕")` drew an empty box; the multiplication
/// sign that survives the probe is a comma-sized cross that reads as a typo at
/// button size. Two line segments have neither problem: they land on the pixel
/// grid at any scale and cannot depend on a font subset at all.
pub fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::Vec2::splat(close_button_size()), egui::Sense::click());
    let resp = named(ui, resp, "Close");
    // Frameless at rest: a header carries one obvious action, and boxing it makes
    // the title bar look like it has a second panel in it.
    let (fill, ring, ink) = if resp.is_pointer_button_down_on() {
        (p().accent_muted, p().accent_ring, p().cream)
    } else if resp.hovered() {
        (p().surface_hover, p().accent, p().cream)
    } else {
        (Color32::TRANSPARENT, Color32::TRANSPARENT, p().text_dim)
    };
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(RADIUS_SM),
        fill,
        Stroke::new(1.0, ring),
        egui::StrokeKind::Inside,
    );
    // The cross keeps its proportions as the target grows: a 32 pt finger-sized
    // button with a 22 pt button's inset would draw a huge ✕, which reads as a
    // different control rather than the same one, bigger.
    let x = rect.shrink(rect.width() * 0.32);
    let stroke = Stroke::new(1.6, ink);
    p.line_segment([x.left_top(), x.right_bottom()], stroke);
    p.line_segment([x.right_top(), x.left_bottom()], stroke);
    paint_focus_ring(ui, &resp, rect, false);
    resp.on_hover_text("Close")
}

/// The ⋮ that opens a row's own actions — **painted, not typed**, and unlike
/// [`close_button`] it carries its frame at rest.
///
/// Painted for the reason the close button is, only more so: *every* vertical
/// ellipsis is missing from the bundled font. `⋮` (U+22EE), `︙` (U+FE19), `⁝`
/// (U+205D), `⠇` (braille) and even the horizontal `⋯` all fail
/// a probe of the loaded fonts, so there is no fallback chain to write here —
/// the survivors are `…` and `☰`, and neither of those is this control. Three
/// dots on the pixel grid have no font subset to depend on.
///
/// **Framed at rest is the whole point.** A place row's actions were reachable
/// only by long-pressing the row, and a long press advertises itself nowhere: the
/// hint was an `on_hover_text`, which a touchscreen never shows. The close button
/// can afford to be frameless because a panel header has one obvious action and a
/// box around it reads as a second panel; a list row is the opposite case, where
/// the affordance *is* the feature.
pub fn kebab_button(ui: &mut egui::Ui) -> egui::Response {
    let side = close_button_size();
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::click());
    let resp = named(ui, resp, "More");
    let (fill, ring, ink) = if resp.is_pointer_button_down_on() {
        (p().accent_muted, p().accent_ring, p().cream)
    } else if resp.hovered() {
        (p().surface_hover, p().accent, p().cream)
    } else {
        (p().surface_raised, p().control_edge, p().text_dim)
    };
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(RADIUS_SM),
        fill,
        Stroke::new(1.0, ring),
        egui::StrokeKind::Inside,
    );
    // Proportional to the target, for the reason the ✕'s inset is: the same
    // absolute dot on a 32 pt finger button and a 22 pt pointer one reads as two
    // different controls rather than as one at two sizes.
    let radius = (side * 0.055).max(1.0);
    let gap = side * 0.22;
    for step in [-1.0, 0.0, 1.0] {
        p.circle_filled(rect.center() + egui::vec2(0.0, step * gap), radius, ink);
    }
    paint_focus_ring(ui, &resp, rect, false);
    // Baked in rather than left to the caller for the reason `close_button`'s is:
    // this control's whole job is to advertise that a row has actions, and its
    // one caller supplying the words is not the same as the control carrying them.
    resp.on_hover_text("Actions")
}

/// The header every right-hand panel opens with: a raised band carrying the
/// panel's own icon, its name, and the close button. Returns whether close was
/// clicked.
///
/// Three things it fixes, all of which the six hand-rolled copies shared. The
/// close glyph was `✕`, which the font cannot draw ([`close_button`]). The
/// heading sat directly on the panel fill, so a panel opened as a wall of one
/// colour with a slightly larger word at the top of it. And nothing tied the
/// header to the control that opened it — the medallion is the dock's disc in
/// its *selected* colours at a smaller radius, so the lit button at the bottom
/// of the screen and the badge at the top of the panel are visibly one thing.
///
/// Depth is a three-part recipe and all three parts matter: a soft shadow under
/// the band (it floats over the content, so the content must be able to pass
/// beneath it), a fill one step up from the panel, and a hairline highlight
/// along the top edge — the lit edge is what turns a lighter rectangle into a
/// raised one.
pub fn panel_header(ui: &mut egui::Ui, icon: &str, title: &str) -> bool {
    panel_header_with(ui, icon, title, |_| {})
}

/// [`panel_header`] plus room for extra actions, laid out right-to-left from the
/// close button (the Places panel's "+ Folder").
pub fn panel_header_with(
    ui: &mut egui::Ui,
    icon: &str,
    title: &str,
    extra: impl FnOnce(&mut egui::Ui),
) -> bool {
    // **No band.** The header used to be a raised, bordered, shadowed box —
    // a panel inside the panel — which made every panel start with a second
    // frame and pushed the title down to body size. It is now the Chats
    // screen's title bar (`chats::list_screen`): a large title straight on the
    // surface and a hairline under it, so all the panels read as one family.
    let avail = ui.available_rect_before_wrap();
    let band = egui::Rect::from_min_size(avail.min, egui::vec2(avail.width(), header_height()));
    ui.painter().line_segment(
        [
            egui::pos2(band.left(), band.bottom() - 0.5),
            egui::pos2(band.right(), band.bottom() - 0.5),
        ],
        Stroke::new(1.0, p().overlay_line),
    );

    let close;
    {
        // The **actions are placed first**, from the right edge, and the title
        // gets what is left. The other way round is what the Places panel shipped
        // as: a truncating `Label` in a left-to-right layout claims the whole
        // remaining width (truncation is how it fits, so it always "fits"), which
        // left the right-to-left child rect empty and let "+ Folder"/"+ Place"
        // overflow leftwards *over* the panel's own name. Same bargain every row
        // in the Places panel makes with its kebab.
        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(band.shrink2(egui::vec2(0.0, HEADER_INSET)))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        row.spacing_mut().item_spacing.x = 8.0;
        close = close_button(&mut row).clicked();
        extra(&mut row);
        // A plain title, as the midas hub titles its pages: the teal medallion
        // that stood before it repeated the dock disc that opened the panel,
        // and was one more accent on a page that should carry one.
        let _ = icon;
        row.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(title)
                        .font(FontId::new(SIZE_PANEL_TITLE, brand_family()))
                        .color(p().cream),
                )
                .truncate()
                .selectable(false),
            );
        });
    }
    ui.advance_cursor_after_rect(band);
    ui.add_space(10.0);
    close
}

// ─── The four states a designer forgets ───────────────────────────────────────
//
// This file owned colour, type, spacing, buttons, headers and checkboxes, and
// stopped there — it exported `ERROR` and `WARN` as *colours* and nothing that
// draws with them. So the app grew eight spinner sites in two idioms, seven
// error surfaces in seven constructions, five empty states in five shapes, and
// three mutually contradictory answers to "this control cannot act here" (hide
// it / grey it / leave it live and let the click do nothing).
//
// The touch consequence is the one that justifies the vocabulary on its own:
// **every disabled-state explanation in the client was an
// `on_disabled_hover_text`, which a touchscreen never shows.** `bookmarks`
// records that fact in a comment about long-press hints and then uses one
// anyway, to explain a button disabled during world load — i.e. on the app's
// primary platform, in the first thirty seconds a user ever spends in it, the
// entire disabled-state layer was addressed to nobody. That answer has to be
// fixable in one place, which means it has to *be* in one place.

/// A section title inside a panel.
///
/// Promoted out of `settings.rs`, which had the only named helper for this while
/// Appearance used a bare `ui.label`, About used `strong().color(CREAM)`, the
/// script editor used a `small()` dim label, and Layers overloaded a checkbox as
/// a heading. Four idioms across ten panels, and only one of them had a name.
///
/// Small capitals in the dim tone, as the contacts screen labels its groups
/// ("ONLINE NOW"): a section title is a signpost, and drawn bold in the brightest
/// tone it competed with the controls under it for the eye.
pub fn section(ui: &mut egui::Ui, title: &str) {
    section_tinted(ui, title, p().text_dim);
}

/// [`section`] in a colour of its own: the admin pages' headings are
/// [`Palette::warn`] gold, the colour Admin wears everywhere it appears
/// (`docs/mockups/home-and-profile/v2`).
///
/// Drawn as the midas hub heads its groups ("Running 4", `.home__head`):
/// sentence case in the semibold face, and a trailing count — a title ending
/// in "(3)" — split off and set in the faint tone after it.
pub fn section_tinted(ui: &mut egui::Ui, title: &str, tone: Color32) {
    let (name, count) = split_count(title);
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            egui::RichText::new(sentence_case(name))
                .font(FontId::new(SIZE_SUB, brand_family()))
                .color(tone),
        );
        if let Some(n) = count {
            ui.label(
                egui::RichText::new(n)
                    .font(FontId::new(SIZE_SUB, brand_family()))
                    .color(p().line),
            );
        }
    });
    ui.add_space(2.0);
}

/// "Asking to join (1)" → ("Asking to join", Some("1")).
fn split_count(title: &str) -> (&str, Option<&str>) {
    if let Some(rest) = title.strip_suffix(')') {
        if let Some((name, n)) = rest.rsplit_once(" (") {
            if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                return (name, Some(n));
            }
        }
    }
    (title, None)
}

/// A title written in capitals by a caller ("ONLINE NOW") in sentence case;
/// anything already mixed-case is left as written, names and all.
fn sentence_case(s: &str) -> String {
    if s.chars().any(char::is_lowercase) {
        return s.to_string();
    }
    let lower = s.to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// What a [`list_row`] carries at its right-hand end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowEnd<'a> {
    /// Opens a page: a chevron.
    Chevron,
    /// A plain count before the chevron ("Contacts 14").
    Count(usize),
    /// Something waiting: the brand-orange pill before the chevron.
    Badge(usize),
    /// A word in place of the chevron ("web page", "Unblock").
    Word(&'a str),
    /// Nothing: an action row.
    Nothing,
}

/// A list of rows on one rounded surface, with hairlines between them: the
/// shape of the You page, Settings and Admin. One surface per group, so a page
/// reads as a few blocks of related things rather than one long column.
pub fn list_group<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let out = egui::Frame::NONE
        .fill(p().card)
        .stroke(egui::Stroke::new(1.0, p().list_line))
        .shadow(card_shadow())
        .corner_radius(egui::CornerRadius::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui)
        })
        .inner;
    ui.add_space(6.0);
    out
}

/// A row of a page's actions as one card: equal cells split by hairlines,
/// each an icon over its word — the actions under a community's name. Returns
/// the cell tapped. `on` cells (a form that is open below) wear the accent.
///
/// It replaces a row of separate outlined buttons, which beside the grouped
/// lists of the Settings panel looked "cheap" (2026-11-10): one card with
/// quiet dividers is the same family as [`list_group`].
pub fn action_strip(ui: &mut egui::Ui, cells: &[(&str, &str, bool)]) -> Option<usize> {
    if cells.is_empty() {
        return None;
    }
    let w = ui.available_width();
    let (card, _) = ui.allocate_exact_size(egui::vec2(w, 60.0), egui::Sense::hover());
    let corner = CornerRadius::same(16);
    ui.painter().add(card_shadow().as_shape(card, corner));
    ui.painter().rect(
        card,
        corner,
        p().card,
        Stroke::new(1.0, p().list_line),
        egui::StrokeKind::Inside,
    );
    let cell_w = w / cells.len() as f32;
    let mut picked = None;
    for (i, (icon, label, on)) in cells.iter().enumerate() {
        let rect = egui::Rect::from_min_size(
            egui::pos2(card.left() + i as f32 * cell_w, card.top()),
            egui::vec2(cell_w, card.height()),
        );
        let resp = ui.interact(
            rect,
            ui.id().with(("action_strip", i)),
            egui::Sense::click(),
        );
        let resp = named(ui, resp, label);
        if resp.clicked() {
            picked = Some(i);
        }
        let painter = ui.painter();
        if resp.hovered() || *on {
            painter.rect_filled(
                rect.shrink(3.0),
                CornerRadius::same(12),
                if *on {
                    p().accent_muted
                } else {
                    p().surface_raised
                },
            );
        }
        if i > 0 {
            painter.vline(
                rect.left(),
                (rect.top() + 12.0)..=(rect.bottom() - 12.0),
                Stroke::new(1.0, p().list_line),
            );
        }
        painter.text(
            egui::pos2(rect.center().x, rect.top() + 21.0),
            egui::Align2::CENTER_CENTER,
            *icon,
            FontId::proportional(18.0),
            p().accent_word,
        );
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 15.0),
            egui::Align2::CENTER_CENTER,
            *label,
            FontId::proportional(SIZE_SUB),
            p().text,
        );
        paint_focus_ring(ui, &resp, rect, false);
    }
    ui.add_space(8.0);
    picked
}

/// One row of a [`list_group`]: an icon on a small raised square, a title, an
/// optional line under it, and `end` on the right. `tone` is the icon's ink
/// ([`Palette::accent`] for most, [`Palette::warn`] on the admin pages, [`Palette::error`] for a row that
/// destroys something, whose title takes the same colour).
pub fn list_row(
    ui: &mut egui::Ui,
    icon: &str,
    title: &str,
    sub: Option<&str>,
    end: RowEnd<'_>,
    tone: Color32,
) -> egui::Response {
    let h = if sub.is_some() { 58.0 } else { 50.0 };
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let resp = named(ui, resp, title);
    let p = ui.painter();
    if resp.hovered() || resp.is_pointer_button_down_on() {
        p.rect_filled(rect, egui::CornerRadius::same(8), self::p().surface_hover);
    }
    p.line_segment(
        [
            egui::pos2(rect.left() + 58.0, rect.bottom() - 0.5),
            egui::pos2(rect.right(), rect.bottom() - 0.5),
        ],
        egui::Stroke::new(1.0, self::p().list_line),
    );
    let chip = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 12.0 + 16.0, rect.center().y),
        egui::Vec2::splat(32.0),
    );
    let chip_fill = if tone == self::p().warn {
        self::p().warn.gamma_multiply(0.14)
    } else {
        self::p().surface_raised
    };
    p.rect_filled(chip, egui::CornerRadius::same(10), chip_fill);
    p.text(
        chip.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(17.0),
        tone,
    );

    // The right-hand end, painted first so the title knows how much room it has.
    let mut right = rect.right() - 12.0;
    let chevron = |p: &egui::Painter, right: &mut f32| {
        p.text(
            egui::pos2(*right, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            crate::systems::icons::CHEVRON_RIGHT,
            egui::FontId::proportional(18.0),
            self::p().line,
        );
        *right -= 24.0;
    };
    match end {
        RowEnd::Chevron => chevron(p, &mut right),
        RowEnd::Count(n) => {
            chevron(p, &mut right);
            let r = p.text(
                egui::pos2(right, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                n.to_string(),
                egui::FontId::proportional(13.0),
                self::p().text_dim,
            );
            right = r.left() - 8.0;
        }
        RowEnd::Badge(n) => {
            chevron(p, &mut right);
            if n > 0 {
                let g = p.layout_no_wrap(
                    if n > 99 { "99+".into() } else { n.to_string() },
                    egui::FontId::proportional(11.5),
                    Color32::WHITE,
                );
                let pill = egui::Rect::from_min_size(
                    egui::pos2(right - g.size().x - 14.0, rect.center().y - 9.5),
                    egui::vec2(g.size().x + 14.0, 19.0),
                );
                p.rect_filled(pill, egui::CornerRadius::same(10), BRAND);
                let gs = g.size();
                p.galley(pill.center() - gs / 2.0, g, Color32::WHITE);
                right = pill.left() - 8.0;
            }
        }
        RowEnd::Word(word) => {
            let r = p.text(
                egui::pos2(right, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                word,
                egui::FontId::proportional(12.5),
                self::p().text_dim,
            );
            right = r.left() - 8.0;
        }
        RowEnd::Nothing => {}
    }

    let x = chip.right() + 12.0;
    let room = (right - x).max(20.0);
    let line = |text: &str, size: f32, color: Color32, y: f32| {
        let mut job = egui::text::LayoutJob::simple_singleline(
            text.to_string(),
            egui::FontId::proportional(size),
            color,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(room);
        let g = p.layout_job(job);
        let gy = y - g.size().y / 2.0;
        p.galley(egui::pos2(x, gy), g, color);
    };
    let title_tone = if tone == self::p().error {
        self::p().error
    } else {
        self::p().text
    };
    match sub {
        Some(sub) => {
            line(title, 14.5, title_tone, rect.center().y - 9.0);
            line(sub, 12.0, self::p().text_dim, rect.center().y + 10.0);
        }
        None => line(title, 14.5, title_tone, rect.center().y),
    }
    paint_focus_ring(ui, &resp, rect, false);
    resp
}

/// A page's title with a way back: the chevron and the name, at the top of a
/// screen inside a panel (You, a Settings page, an Admin page). Answers whether
/// back was pressed.
///
/// It also carries the panel's close, on the right, as [`panel_header`] does.
/// A page several levels deep (Community ▸ You ▸ Settings ▸ Graphics) is the
/// whole screen on a phone, and with only a chevron the way back to the map was
/// one tap per level. The close cannot be returned from here, since the page
/// does not know which panel it is in; it is raised with
/// [`request_panel_close`] and the panel that owns the page reads it back with
/// [`take_panel_close`] after its body has drawn.
pub fn back_title(ui: &mut egui::Ui, title: &str) -> bool {
    back_title_with(ui, title, |_| {})
}

/// [`back_title`] plus room for a page's own actions, laid out right-to-left
/// from the close (the calendar's + and ⋯).
pub fn back_title_with(ui: &mut egui::Ui, title: &str, extra: impl FnOnce(&mut egui::Ui)) -> bool {
    let mut back = false;
    let row_h = close_button_size().max(30.0);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), row_h),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            // The close is placed first and the title takes what is left, for
            // the reason `panel_header_with` gives: a truncating label laid out
            // first claims the whole row.
            if close_button(ui).clicked() {
                request_panel_close(ui.ctx());
            }
            extra(ui);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let resp = ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(crate::systems::icons::BACK)
                                .size(22.0)
                                .color(p().cream),
                        )
                        .frame(false)
                        .min_size(egui::vec2(row_h, row_h)),
                    )
                    .on_hover_text("Back");
                back = page_back(ui.ctx(), resp.clicked());
                ui.add(
                    egui::Label::new(egui::RichText::new(title).size(18.0).color(p().cream))
                        .truncate()
                        .selectable(false),
                );
            });
        },
    );
    ui.add_space(6.0);
    back
}

/// Start a new row of a `horizontal_wrapped` layout if something `width` wide
/// will not fit in what is left of this one.
///
/// A wrapped layout wraps a *widget*, which knows its size before it is placed.
/// A `Frame` is placed first and measured after, so a row of framed chips never
/// wraps: it runs off the side and pushes whatever holds it wider. The person
/// card's three facts grew the card to 456 pt on a 275 pt phone (2026-10-31),
/// half of it off the screen on each side.
pub fn wrap_before(ui: &mut egui::Ui, width: f32) {
    let at_row_start = ui.cursor().min.x <= ui.max_rect().min.x + 0.5;
    // `available_width` is the whole row in a wrapping layout; what is left of
    // this one is `before_wrap`.
    if !at_row_start && ui.available_size_before_wrap().x < width {
        ui.end_row();
    }
}

/// Where [`request_panel_close`] leaves its request: the pass it was made in.
fn panel_close_id() -> egui::Id {
    egui::Id::new("theme::panel_close")
}

/// Ask the panel this page is drawn in to close — the close of a page header
/// that cannot name its panel ([`back_title`]).
pub fn request_panel_close(ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(panel_close_id(), pass));
}

/// Whether a page in this pass asked its panel to close, consuming the ask.
///
/// Keyed to the pass, so a request no panel read (the page was drawn by
/// something that does not call this) dies with its frame instead of closing
/// whichever panel asks next.
pub fn take_panel_close(ctx: &egui::Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.remove_temp::<u64>(panel_close_id()))
        .is_some_and(|asked| asked == pass)
}

/// Where a page's back chevron says it was drawn, and where the dismissal
/// ladder leaves a press of Back or Escape for it: the pass of each.
fn page_back_seen_id() -> egui::Id {
    egui::Id::new("theme::page_back_seen")
}
fn page_back_pending_id() -> egui::Id {
    egui::Id::new("theme::page_back_pending")
}

/// A page's back chevron: whether it was `clicked`, or Back / Escape was
/// pressed for it.
///
/// Every chevron inside a panel goes through this, so the dismissal ladder
/// (`systems::dismiss`) steps back **one page** — Contacts to You, You to
/// Community — rather than closing the whole stack at once, which is what a
/// press did while the pages kept their own state out of the ladder's sight.
/// The page still decides where back goes; this only lets a key press it.
pub fn page_back(ctx: &egui::Context, clicked: bool) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(page_back_seen_id(), pass));
    let pressed = ctx
        .data_mut(|d| d.remove_temp::<u64>(page_back_pending_id()))
        // The ladder runs after the panel chain, so its press is read in the
        // next pass; anything older belonged to a page no longer drawn.
        .is_some_and(|at| pass.saturating_sub(at) <= 2);
    clicked || pressed
}

/// Whether a page with a back chevron was drawn in this pass or the last.
#[must_use]
pub fn page_back_shown(ctx: &egui::Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<u64>(page_back_seen_id()))
        .is_some_and(|at| pass.saturating_sub(at) <= 1)
}

/// Press the shown page's back chevron (the dismissal ladder's page rung).
pub fn press_page_back(ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        d.remove_temp::<u64>(page_back_seen_id());
        d.insert_temp(page_back_pending_id(), pass);
    });
}

/// One control with several positions: a raised pill with equal-width
/// segments, the chosen one on the [`Palette::accent`] plate in [`Palette::on_accent`] — the Chats
/// screen's toggle (`chats::segmented`) made general. Returns whether the
/// value changed; `value` is written only on a click, so a caller holding a
/// resource does not mark it changed every frame.
///
/// It replaced rows of `selectable_value`s, which wrapped as soon as a panel
/// was narrow ("Full detail" alone on a second line) and read as loose words
/// rather than one choice. Labels that do not fit their segment are set
/// smaller, down to [`SIZE_SUB`]; past that the options take two rows.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    if options.is_empty() {
        return false;
    }
    let gap = 2.0;
    let width = ui.available_width();
    let n = options.len();
    // How wide a label is, measured in the chosen option's (wider) brand face
    // so that choosing one never makes it spill.
    let label_w = |label: &str, size: f32| {
        ui.ctx().fonts_mut(|f| {
            f.layout_no_wrap(
                label.to_string(),
                FontId::new(size, brand_family()),
                p().text,
            )
            .size()
            .x
        })
    };
    let seg_w_for = |cols: usize| (width - 6.0 - gap * (cols as f32 - 1.0)) / cols as f32;
    let fits = |cols: usize, size: f32| {
        let seg_w = seg_w_for(cols);
        options
            .iter()
            .all(|(_, label)| label_w(label, size) <= seg_w - 10.0)
    };
    // One row while every label fits it at [`SIZE_SUB`] or larger; otherwise
    // two rows, half the options in each, at the same sizes. Shrinking four
    // options into one row on a phone made them unreadable, and wrapping
    // inside a segment broke "Automatic" mid-word (2026-11-10).
    let cols = if fits(n, SIZE_SUB) { n } else { n.div_ceil(2) };
    let rows = n.div_ceil(cols);
    let seg_w = seg_w_for(cols);
    let mut size = SIZE_BODY - 1.0;
    while size > SIZE_TINY && !fits(cols, size) {
        size -= 0.5;
    }
    let row_h = row_height().max(30.0) - 6.0;
    let height = rows as f32 * row_h + (rows as f32 - 1.0) * gap + 6.0;
    let (rect, track) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let inner = rect.shrink(3.0);
    // Keyed on the track's own auto id: two rows with the same first label
    // ("Automatic") in one tab must not share segment ids.
    let base = track.id;

    // A card like the lists around it, the chosen option on the accent plate
    // (2026-11-10: the hub's deep well with a raised card on it was "ugly", of
    // the Clouds switch).
    let p = ui.painter().clone();
    p.rect_filled(rect, CornerRadius::same(RADIUS_LG), self::p().card);
    p.rect_stroke(
        rect,
        CornerRadius::same(RADIUS_LG),
        Stroke::new(1.0, self::p().list_line),
        egui::StrokeKind::Inside,
    );
    let mut changed = false;
    for (i, (option, label)) in options.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        let r = egui::Rect::from_min_size(
            egui::pos2(
                inner.left() + col as f32 * (seg_w + gap),
                inner.top() + row as f32 * (row_h + gap),
            ),
            egui::vec2(seg_w, row_h),
        );
        let resp = ui
            .interact(r, base.with(i), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let on = *value == *option;
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, *label)
        });
        let (fill, ink) = if on {
            (self::p().accent, self::p().on_accent)
        } else if resp.is_pointer_button_down_on() {
            (self::p().surface_hover, self::p().cream)
        } else if resp.hovered() {
            (self::p().surface_raised, self::p().cream)
        } else {
            (Color32::TRANSPARENT, self::p().text_dim)
        };
        p.rect_filled(r, CornerRadius::same(RADIUS_LG - 3), fill);
        let font = FontId::new(
            size,
            if on {
                brand_family()
            } else {
                FontFamily::Proportional
            },
        );
        p.text(r.center(), egui::Align2::CENTER_CENTER, *label, font, ink);
        paint_focus_ring(ui, &resp, r, false);
        if resp.clicked() && !on {
            *value = *option;
            changed = true;
        }
    }
    changed
}

/// A line under a control stating a constraint or a stored intention — *not* a
/// failure. Plain dim text rather than a warning colour: it is a fact about the
/// build or the world, not something the user did wrong.
pub fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(SIZE_CHIP)
            .color(p().text_dim)
            .italics(),
    );
}

/// The body of a list with nothing in it.
///
/// `hint` is **not optional**, and that is the whole design: "No places yet" on
/// its own is a dead end, and the Places panel's existing empty state — which
/// names the two buttons and says what each does — is the model this generalises.
/// Three lists in the client currently draw literally nothing when empty (an
/// empty chat channel, an empty folder, the texture library), which is
/// indistinguishable from a panel that failed to load.
pub fn empty_state(ui: &mut egui::Ui, headline: &str, hint: &str) {
    ui.add_space(12.0);
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(headline)
                .size(SIZE_BODY)
                .color(p().text_dim),
        );
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(egui::RichText::new(hint).size(SIZE_SUB).color(p().text_dim)).wrap(),
        );
    });
    ui.add_space(12.0);
}

/// One in-progress row: spinner plus what is being waited for.
///
/// **The** spinner idiom. A bare `ui.spinner()` takes its colour from
/// `noninteractive.fg_stroke` and its size from the row, which is why the sign-in
/// card hand-rolled a themed one and the other seven sites did not — two
/// spinners, in one app, for one meaning.
pub fn busy(ui: &mut egui::Ui, label: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(SIZE_BODY).color(p().accent));
        ui.label(
            egui::RichText::new(label)
                .size(SIZE_SUB)
                .color(p().text_dim),
        );
    });
}

/// [`busy`] with a count, for the one kind of wait that can say how far along it
/// is. The Route panel's tile read is the only place in the client that has this
/// number today; it should not be the only place that can show it.
pub fn busy_progress(ui: &mut egui::Ui, label: &str, done: usize, total: usize) {
    busy(ui, &format!("{label} {done}/{total}"));
}

/// An inline failure with a Dismiss, returning `true` when dismissed so the
/// caller clears its own `Option<String>`.
///
/// On a faint [`Palette::error`]-tinted chip rather than as bare coloured text, so that an
/// error can never again be styled as a footnote — the Search panel reported a
/// geocoding failure through `ui.weak()`, which is the *quietest* tone in the
/// palette, for the one string that most needs to be read.
#[must_use]
pub fn inline_error(ui: &mut egui::Ui, message: &str) -> bool {
    let mut dismissed = false;
    egui::Frame::NONE
        .fill(p().error.gamma_multiply(0.16))
        .stroke(Stroke::new(1.0, p().error.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(RADIUS_SM))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(message).size(SIZE_SUB).color(p().error))
                        .wrap(),
                );
            });
            // One row's height, not `with_layout`: in a vertical parent that
            // takes every pixel left below it, and on the chat list the chip
            // swallowed the screen with Dismiss floating in the middle.
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    dismissed = action_button(ui, "Dismiss").clicked();
                },
            );
        });
    dismissed
}

/// A control that cannot act, and the reason — **in a form a phone can read.**
///
/// Under a pointer this is `on_disabled_hover_text`, which is what the ten
/// existing `add_enabled` sites do (where they explain themselves at all — most
/// grey a control and say nothing). Under a finger the reason is drawn as a
/// [`note`] beneath the control instead, because a tooltip on a touchscreen is a
/// sentence addressed to nobody. One helper, so that answer is decided once.
pub fn action_enabled_because(
    ui: &mut egui::Ui,
    enabled: bool,
    label: impl Into<egui::WidgetText>,
    why_disabled: &str,
) -> egui::Response {
    let resp = action_enabled(ui, enabled, label);
    if enabled {
        return resp;
    }
    if touch_layout() {
        note(ui, why_disabled);
        resp
    } else {
        resp.on_disabled_hover_text(why_disabled)
    }
}

/// How long an armed [`confirm_button`] waits before disarming itself.
const CONFIRM_ARMED_S: f64 = 3.0;

/// A destructive action that takes two presses: the first arms the button and
/// swaps its label, the second does the thing. Returns `true` only on the second.
///
/// **Nothing in this client confirms anything.** Deleting a place removes two
/// JPEGs from `platform::storage`; "Remove the photo" sits one row under "View
/// the photo" with no separator, which on a phone is a ~20 pt mis-tap between
/// looking at a picture and destroying it; the script editor's Stop deletes a
/// script off the server; and there is no undo for any of them.
///
/// A modal would be the conventional answer and is the wrong shape here: this
/// app draws its panels into one shared root `Ui` with a hand-built ordering
/// chain, and a floating confirm dialog is a new `Area` fighting that chain for
/// a decision that is one bit wide. Arming in place costs no layer, cannot be
/// occluded, and puts the confirmation exactly where the finger already is.
///
/// State lives in `ui.memory` under `id`, so a caller stays a one-liner and no
/// panel needs a new field; the arm times out so a button left armed by a stray
/// tap does not lie in wait.
#[must_use]
pub fn confirm_button(ui: &mut egui::Ui, id: egui::Id, label: &str, confirm_label: &str) -> bool {
    let now = ui.input(|i| i.time);
    let armed_at: Option<f64> = ui.memory(|m| m.data.get_temp(id));
    let armed = armed_at.is_some_and(|t| now - t < CONFIRM_ARMED_S);

    let resp = if armed {
        ui.add(
            egui::Button::new(
                egui::RichText::new(confirm_label)
                    .color(p().on_status)
                    .strong(),
            )
            .fill(p().error)
            .corner_radius(CornerRadius::same(RADIUS_SM)),
        )
    } else {
        action_button(ui, label)
    };

    if resp.clicked() {
        if armed {
            ui.memory_mut(|m| m.data.remove::<f64>(id));
            return true;
        }
        ui.memory_mut(|m| m.data.insert_temp(id, now));
        return false;
    }
    // Repaint while armed, or the label would sit on "Delete?" until some other
    // event happened to redraw the panel — a timeout nobody can see has not
    // timed out.
    if armed {
        ui.ctx().request_repaint();
    } else if armed_at.is_some() {
        ui.memory_mut(|m| m.data.remove::<f64>(id));
    }
    false
}

/// A checkbox whose box is square. egui derives the checkbox frame's rounding
/// from the shared widget visuals ([`RADIUS`], sized for buttons), and on a
/// ~14 pt box that radius reads as a circle — an unchecked toggle then looks
/// like a radio button, which promises "pick one" instead of "on/off". Scopes
/// the rounding down for just this widget; every other visual stays the
/// theme's. Use this, not `ui.checkbox`, everywhere.
pub fn checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    // A ticked box is a teal plate with a dark tick: "on" in the same
    // treatment as every other engaged control. egui strokes the tick in the
    // label's colour, so the box takes the accent through the style and the
    // tick is painted again over it in [`Palette::ink`].
    let was = *checked;
    ui.scope(|ui| {
        let square = CornerRadius::same(RADIUS_SM);
        let w = &mut ui.style_mut().visuals.widgets;
        for v in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
        ] {
            v.corner_radius = square;
            if was {
                v.bg_fill = p().accent;
                v.bg_stroke = Stroke::new(1.0, p().accent_ring);
            }
        }
        let resp = ui.checkbox(checked, text);
        if was && *checked {
            let size = ui.spacing().icon_width_inner;
            let c = egui::pos2(
                resp.rect.left() + ui.spacing().icon_width / 2.0,
                resp.rect.center().y,
            );
            let r = egui::Rect::from_center_size(c, egui::Vec2::splat(size));
            ui.painter().add(egui::Shape::line(
                vec![
                    egui::pos2(r.left(), r.center().y),
                    egui::pos2(r.center().x, r.bottom()),
                    egui::pos2(r.right(), r.top()),
                ],
                Stroke::new(2.0, p().on_accent),
            ));
        }
        resp
    })
    .inner
}

/// A password field, with the eye that reveals what is in it.
///
/// **Why every password field in this client needs one.** A masked field is a
/// row of dots you cannot proof-read, which is a mild annoyance with a hardware
/// keyboard and a dead end with a thumb: the two platforms this client runs on
/// with no system IME type through an on-screen keyboard
/// ([`crate::systems::onscreen_keyboard`]) with no autocorrect and no key
/// preview, so a mistyped character is invisible, unlocatable, and answered by a
/// server that will only say the whole thing was wrong.
///
/// The reveal is **per field and per frame-run, never persisted**: it lives in
/// egui's temporary memory under the field's own id, so it is gone by the next
/// launch and a second password field on the same screen is not revealed by the
/// first.
///
/// The non-obvious part is the focus restore. Tapping the eye is a click that is
/// not on the field, and egui answers those by taking the field's focus away
/// (`SurrenderFocusOn::Clicks`) — which on a phone takes the on-screen keyboard
/// down with it, so revealing your password would cost you the keyboard you were
/// typing it with. Putting the focus back has to happen in the same pass, before
/// the keyboard's own system runs and reads `text_edit_focused()`; every UI
/// system runs before it, by construction (`gui::plugin`, the on-screen keyboard's `.after` edges).
pub fn password_field(ui: &mut egui::Ui, text: &mut String) -> egui::Response {
    let eye = close_button_size();
    let gap = ui.spacing().item_spacing.x;
    ui.horizontal(|ui| {
        // The eye is paid for out of the field, not added to the row: these sit
        // in a labelled two-column layout on a 274 pt phone viewport, and a row
        // that grows by its own button pushes the field off the card.
        let width = (ui.available_width() - eye - gap).max(40.0);
        // Keyed off this row's own `Ui`, not the field's auto-id: the field's id
        // is only known after it has been shown, and this has to be read before.
        let id = ui.id().with("password_shown");
        let shown = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
        let field = crate::systems::text_touch::field(
            ui,
            egui::TextEdit::singleline(text)
                .password(!shown)
                .desired_width(width),
        );

        let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(eye), egui::Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                ui.is_enabled(),
                shown,
                "Show the password",
            )
        });
        let ink = if resp.is_pointer_button_down_on() || shown {
            p().accent
        } else {
            p().text_dim
        };
        paint_eye(ui.painter(), rect, shown, ink);
        paint_focus_ring(ui, &resp, rect, false);
        if resp.clicked() {
            ui.data_mut(|d| d.insert_temp(id, !shown));
            // Only when it was ours to lose: tapping the eye of a field nobody is
            // typing into must not pull focus into it, or the on-screen keyboard
            // slides up over a card the user was only reading.
            if field.has_focus() || field.lost_focus() {
                crate::systems::onscreen_keyboard::restore_focus(ui.ctx(), field.id);
            }
        }
        let _ = resp.on_hover_text(if shown {
            "Hide the password"
        } else {
            "Show the password"
        });
        // The *field's* response, not a union with the eye's: callers test it for
        // `changed()` and `lost_focus()` to know whether the password was edited
        // or submitted, and a union would answer those for a look at it.
        field
    })
    .inner
}

/// An eye, painted rather than lettered.
///
/// The icon font ([`crate::systems::icons`]) is the right answer for a glyph and
/// is *not* available for this one: it is subset from a manifest, so
/// `visibility` / `visibility_off` are not in the shipped file, and putting them
/// there means re-running `tools/subset-icon-font.py` and committing a new LFS
/// binary for two characters. So it is drawn, on the same terms as
/// [`close_button`]'s cross and the on-screen keyboard's select-all band. Add the
/// icons and delete this the next time that font is re-cut.
///
/// Struck through when the password is *hidden*, which is the state the field
/// starts in — so the mark says what tapping it will do, not what is happening.
fn paint_eye(p: &egui::Painter, rect: egui::Rect, shown: bool, ink: Color32) {
    let c = rect.center();
    let r = rect.width() * 0.30;
    let stroke = Stroke::new(1.4, ink);
    // The almond, as two arcs approximated by short segments — epaint has no
    // quadratic-curve primitive on the simple painter, and a circle would read as
    // a target rather than an eye.
    let arc = |up: f32| {
        let n = 10;
        let pts: Vec<egui::Pos2> = (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                let x = c.x + (t - 0.5) * 2.0 * r;
                // A parabola through (±r, 0) with its apex at 0.62 r.
                let y = c.y - up * 0.62 * r * (1.0 - (2.0 * t - 1.0).powi(2));
                egui::pos2(x, y)
            })
            .collect();
        pts
    };
    p.add(egui::Shape::line(arc(1.0), stroke));
    p.add(egui::Shape::line(arc(-1.0), stroke));
    p.circle_filled(c, r * 0.30, ink);
    if !shown {
        p.line_segment(
            [
                egui::pos2(c.x - r, c.y + r * 0.7),
                egui::pos2(c.x + r, c.y - r * 0.7),
            ],
            stroke,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::egui_manager::UnpaintedPasses as _;

    /// Relative luminance per WCAG 2.x, on a `Color32`'s sRGB bytes.
    fn luminance(c: Color32) -> f64 {
        let ch = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Both palettes, named, for the tests that hold in each.
    const BOTH: [(&str, &Palette); 2] = [("dark", &DARK), ("light", &LIGHT)];

    /// The palette's whole claim is that it is *legible*, and a colour is very
    /// easy to nudge into looking nicer and reading worse. Pin the pairs that
    /// actually occur: text on a panel, text on a selected row, the ink on the
    /// accent plate — in both themes.
    #[test]
    fn text_colours_clear_their_backgrounds() {
        for (theme, c) in BOTH {
            for (name, fg, bg, min) in [
                ("body on panel", c.text, c.surface, 4.5),
                ("body on raised", c.text, c.surface_raised, 4.5),
                ("dim on panel", c.text_dim, c.surface, 4.5),
                ("heading on panel", c.cream, c.surface, 4.5),
                ("link on panel", c.accent_text, c.surface, 4.5),
                ("selected row", c.cream, c.accent_muted, 4.5),
                ("label on pressed button", c.cream, c.accent_muted, 4.5),
                ("accent word on its tag", c.accent_word, c.accent_muted, 4.5),
                ("ink on an engaged plate", c.on_accent, c.accent, 4.5),
                // The three fills [`primary_button_sized`] moves between, all
                // labelled in the plate's ink.
                ("primary label, hovered", c.on_accent, c.accent_lift, 4.5),
                ("primary label, pressed", c.on_accent, c.accent_pressed, 4.5),
                ("warning on panel", c.warn, c.surface, 4.5),
                ("error on panel", c.error, c.surface, 4.5),
                ("ink on an armed confirm", c.on_status, c.error, 4.5),
                ("ink on a warning plate", c.on_status, c.warn, 4.5),
                // Not text: a fill, a rule or a focus ring, which WCAG puts at 3:1.
                ("focus ring on panel", c.accent, c.surface, 3.0),
            ] {
                let got = contrast(fg, bg);
                assert!(got >= min, "{theme}: {name}: contrast {got:.2} < {min}");
            }
        }
        // The script editor's code box is dark in both themes (`script_editor`
        // gives it the dark visuals), so its colours are measured once, on
        // the dark ink.
        let ink = DARK.ink;
        for (name, fg) in [
            ("code text", DARK.text),
            ("code keyword", CODE_KEYWORD),
            ("code string", CODE_STRING),
            ("code literal", CODE_LITERAL),
            ("code comment", CODE_COMMENT),
            ("code api", CODE_API),
            ("code call", CODE_CALL),
        ] {
            let got = contrast(fg, ink);
            assert!(got >= 4.5, "{name}: contrast {got:.2} < 4.5");
        }
    }

    /// Composite a translucent [`Color32`] over an opaque one.
    ///
    /// `Color32` stores its channels **premultiplied** (`from_rgba_unmultiplied`
    /// multiplies on construction), so the source term is already `rgb·a` and
    /// this is plain `src + dst·(1 − a)`.
    fn over(fg: Color32, bg: Color32) -> Color32 {
        let inv = 1.0 - f32::from(fg.a()) / 255.0;
        let ch = |f: u8, b: u8| (f32::from(f) + f32::from(b) * inv).round() as u8;
        Color32::from_rgb(ch(fg.r(), bg.r()), ch(fg.g(), bg.g()), ch(fg.b(), bg.b()))
    }

    /// The basemap under every overlay box, at its brightest.
    ///
    /// Not an adversarial extreme — it is *the* case. The imagery is a map, not
    /// a satellite photo: pale beige land on near-white paper, which is the same
    /// fact `globe::GLOBE_DEEP_ALBEDO` exists to deal with. A translucent chrome
    /// box tested only against the panel fill it borrows its colour from has
    /// been tested against the one background it never has.
    const BRIGHT_MAP: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
    /// The same at the other end — a night city, a dark aerial, deep water.
    const DARK_MAP: Color32 = Color32::from_rgb(0x18, 0x18, 0x18);

    /// The panel is solid and cool, never warmer (the brown a mix of the brand
    /// orange produced, red above blue, is what this pins against).
    ///
    /// **Dark**, it is visibly teal rather than black, and the blocks in it are
    /// close to it, since a jump in tone between the two was "too much
    /// contrast" (2026-11-10): a block is told apart by its hairline and its
    /// shadow. **Light** takes the hub's white cards on its page instead, a
    /// step further apart (1.10:1), so the quiet-blocks rule is the dark
    /// theme's.
    #[test]
    fn the_panel_is_a_dark_teal_with_quiet_blocks() {
        for (theme, c) in BOTH {
            let panel = c.panel;
            assert_eq!(panel.a(), 255, "{theme}: the panel is solid: {panel:?}");
            assert!(
                panel.r() <= panel.b() && panel.r() <= panel.g(),
                "{theme}: the panel has a warm cast: {panel:?}"
            );
        }
        assert!(
            DARK.panel.g() > DARK.panel.r() + 8,
            "the panel has gone black: {:?}",
            DARK.panel
        );
        assert!(
            contrast(DARK.card, DARK.panel) < 1.08,
            "the blocks jump off the panel: {}",
            contrast(DARK.card, DARK.panel)
        );
    }

    /// **Every surface a colour can land on, not just the panel fill.**
    ///
    /// [`text_colours_clear_their_backgrounds`] pins the pairs against
    /// [`Palette::surface`] and the accent fills. That is the easy half of the
    /// palette: the ramp exists precisely so things *can* sit on the raised and
    /// hover steps, and egui puts them there without being asked —
    /// `weak_text_color` inside an `open` menu is the dim tone on the hover
    /// step, which measured 4.40 and is the failure this test was written for.
    ///
    /// The overlay is the other half, and it was worse, because its background
    /// is not in this file at all: it is the map, at both ends of its
    /// brightness. The boxes are solid now, so the composite is the fill; the
    /// rows stay so a box made translucent again is measured over the map.
    #[test]
    fn text_clears_every_surface_it_can_land_on() {
        for (theme, c) in BOTH {
            let over_bright = over(c.overlay, BRIGHT_MAP);
            let over_dark = over(c.overlay, DARK_MAP);
            let glass_bright = over(c.glass, BRIGHT_MAP);
            let glass_dark = over(c.glass, DARK_MAP);
            for (name, fg, bg, min) in [
                // The raised step: buttons under the pointer, striped rows.
                ("dim on raised", c.text_dim, c.surface_raised, 4.5),
                ("heading on raised", c.cream, c.surface_raised, 4.5),
                ("link on raised", c.accent_text, c.surface_raised, 4.5),
                ("warning on raised", c.warn, c.surface_raised, 4.5),
                ("error on raised", c.error, c.surface_raised, 4.5),
                (
                    "held-state rim on raised",
                    c.accent_bright,
                    c.surface_raised,
                    4.5,
                ),
                // The glyph on an engaged plate, the one place in the dark
                // theme where dark type sits on a light fill.
                ("ink on an engaged plate", c.on_accent, c.accent, 4.5),
                (
                    "ink on an engaged plate's rim",
                    c.on_accent,
                    c.accent_ring,
                    4.5,
                ),
                // The hover step — and an *open* menu button sits here at rest.
                ("body on hover", c.text, c.surface_hover, 4.5),
                ("dim on hover", c.text_dim, c.surface_hover, 4.5),
                ("heading on hover", c.cream, c.surface_hover, 4.5),
                ("link on hover", c.accent_text, c.surface_hover, 4.5),
                ("error on hover", c.error, c.surface_hover, 4.5),
                // Text-edit wells, a segmented control's track.
                ("body on ink", c.text, c.ink, 4.5),
                ("dim on ink", c.text_dim, c.ink, 4.5),
                ("link on ink", c.accent_text, c.ink, 4.5),
                // The panels, and the cards in them.
                ("body on a panel", c.text, c.panel, 4.5),
                ("dim on a panel", c.text_dim, c.panel, 4.5),
                ("hairline on a panel", c.line, c.panel, 3.0),
                ("body on a card", c.text, c.card, 4.5),
                ("dim on a card", c.text_dim, c.card, 4.5),
                // The boxes over the map, against the map — both ends of it.
                (
                    "body on an overlay over bright map",
                    c.text,
                    over_bright,
                    4.5,
                ),
                (
                    "dim on an overlay over bright map",
                    c.text_dim,
                    over_bright,
                    4.5,
                ),
                (
                    "heading on an overlay over bright map",
                    c.cream,
                    over_bright,
                    4.5,
                ),
                (
                    "link on an overlay over bright map",
                    c.accent_text,
                    over_bright,
                    4.5,
                ),
                (
                    "warning on an overlay over bright map",
                    c.warn,
                    over_bright,
                    4.5,
                ),
                (
                    "error on an overlay over bright map",
                    c.error,
                    over_bright,
                    4.5,
                ),
                ("body on an overlay over dark map", c.text, over_dark, 4.5),
                (
                    "dim on an overlay over dark map",
                    c.text_dim,
                    over_dark,
                    4.5,
                ),
                // The panels and windows, over the map.
                ("body on a panel over bright map", c.text, glass_bright, 4.5),
                (
                    "dim on a panel over bright map",
                    c.text_dim,
                    glass_bright,
                    4.5,
                ),
                (
                    "heading on a panel over bright map",
                    c.cream,
                    glass_bright,
                    4.5,
                ),
                (
                    "link on a panel over bright map",
                    c.accent_text,
                    glass_bright,
                    4.5,
                ),
                (
                    "warning on a panel over bright map",
                    c.warn,
                    glass_bright,
                    4.5,
                ),
                (
                    "error on a panel over bright map",
                    c.error,
                    glass_bright,
                    4.5,
                ),
                ("body on a panel over dark map", c.text, glass_dark, 4.5),
                ("dim on a panel over dark map", c.text_dim, glass_dark, 4.5),
            ] {
                let got = contrast(fg, bg);
                assert!(got >= min, "{theme}: {name}: contrast {got:.2} < {min}");
            }
        }

        // The map's own labels are dark in both themes (`LABEL_CHIP`,
        // `LABEL_HALO`): a few words on an arbitrary tile, and the text on
        // them is the dark palette's.
        let chip_bright = over(LABEL_CHIP, BRIGHT_MAP);
        let halo_bright = over(LABEL_HALO, BRIGHT_MAP);
        let halo_dark = over(LABEL_HALO, DARK_MAP);
        for (name, fg, bg) in [
            ("chip label over bright map", DARK.cream, chip_bright),
            ("chip dim over bright map", DARK.text_dim, chip_bright),
            ("street name over bright map", LABEL_INK, halo_bright),
            ("street name over dark map", LABEL_INK, halo_dark),
        ] {
            let got = contrast(fg, bg);
            assert!(got >= 4.5, "{name}: contrast {got:.2} < 4.5");
        }
    }

    /// **The state ladder moves one way.** Rest → hover → pressed → engaged,
    /// each a step further from the rest surface — lighter on the dark theme,
    /// darker on the light one: lightness is the channel that still says "this
    /// control is doing something" to someone who cannot see the hue, and a
    /// ladder that doubles back is a UI where two different states look the
    /// same.
    #[test]
    fn the_state_ladder_rises() {
        for (theme, c) in BOTH {
            let rungs = [
                ("rest", c.surface_raised),
                ("hover", c.surface_hover),
                ("pressed", c.accent_muted),
                ("engaged", c.accent),
            ];
            // +1 when the ladder climbs (dark), -1 when it descends (light).
            let way = (luminance(c.accent) - luminance(c.surface_raised)).signum();
            for pair in rungs.windows(2) {
                let ((lo_name, lo), (hi_name, hi)) = (pair[0], pair[1]);
                assert!(
                    (luminance(hi) - luminance(lo)) * way > 0.0,
                    "{theme}: {hi_name} ({:.3}) is not a step past {lo_name} ({:.3})",
                    luminance(hi),
                    luminance(lo)
                );
            }
            // And the two ends are far enough apart to be told apart at a
            // glance, which "strictly monotonic" alone does not promise.
            let got = contrast(c.accent, c.surface_raised);
            assert!(
                got >= 3.0,
                "{theme}: engaged against rest: contrast {got:.2} < 3.0"
            );
        }
    }

    /// **A control has to be findable before its label matters**, and WCAG puts
    /// that boundary at 3:1 (1.4.11) — the same bar as a focus ring, for the same
    /// reason: it is the only thing saying *here is a thing you can press*.
    ///
    /// A button at rest is the raised step on the surface, about 1.2:1 of fill
    /// difference, framed by a [`Palette::line`] hairline that used to measure
    /// 1.92:1 on the dark theme. Neither channel identified the control.
    ///
    /// Two pairs are deliberately *not* asserted: the hover step against the
    /// raised one is a *change* of state rather than the identification of a
    /// control, and the accent ring arriving with it is what carries hover; and
    /// the line against the hover step only ever appears on an `open` menu
    /// button, where the popup below it is the thing being looked at.
    #[test]
    fn a_control_is_distinguishable_from_the_surface_it_sits_on() {
        for (theme, c) in BOTH {
            let v = visuals_of(c);
            // A button at rest is edged in the hub's soft border, not a 3:1
            // line: the person called a page of 3:1-edged buttons "cheap"
            // (2026-11-10), and the hub tells its buttons by plate, shape and
            // word. The edge must still be there, so it is held to a floor.
            for (name, got) in [
                (
                    "a button's border on a panel",
                    contrast(v.widgets.inactive.bg_stroke.color, c.surface),
                ),
                (
                    "a button's border on the page",
                    contrast(v.widgets.inactive.bg_stroke.color, c.panel),
                ),
            ] {
                assert!(got >= 1.3, "{theme}: {name}: contrast {got:.2} < 1.3");
            }
            for (name, got) in [
                (
                    "the hover ring against the fill it encloses",
                    contrast(v.widgets.hovered.bg_stroke.color, c.surface_hover),
                ),
                (
                    "the pressed ring against the fill it encloses",
                    contrast(
                        v.widgets.active.bg_stroke.color,
                        v.widgets.active.weak_bg_fill,
                    ),
                ),
                (
                    "the pressed ring against the panel outside it",
                    contrast(v.widgets.active.bg_stroke.color, c.surface),
                ),
                (
                    "the engaged plate against the panel it sits on",
                    contrast(c.accent, c.surface),
                ),
                (
                    "the glyph on an engaged plate",
                    contrast(c.on_accent, c.accent),
                ),
            ] {
                assert!(got >= 3.0, "{theme}: {name}: contrast {got:.2} < 3.0");
            }

            // **The focus ring is two rings and has to be checked as two.** It
            // is drawn *outside* the control, so its background is the map at
            // any brightness, or a panel, or the plate of the control it
            // encircles. `paint_focus_ring` answers that with the ink under
            // the strongest text tone — dark under light on the dark theme,
            // the other way round on the light one — and the property is that
            // **at least one of them clears 3:1 on every background**.
            for (name, bg) in [
                ("a panel", c.surface),
                (
                    "an overlay disc over bright map",
                    over(c.overlay, BRIGHT_MAP),
                ),
                ("an overlay disc over dark map", over(c.overlay, DARK_MAP)),
                ("the plate of an engaged disc", c.accent),
                ("bare bright basemap", BRIGHT_MAP),
                ("bare dark basemap", DARK_MAP),
            ] {
                let best = contrast(c.cream, bg).max(contrast(c.ink, bg));
                assert!(
                    best >= 3.0,
                    "{theme}: the keyboard-focus ring on {name}: neither half clears \
                     3:1 (best {best:.2})"
                );
            }
        }
    }

    /// The palette's own prose is a *claim*, and prose does not recompute.
    ///
    /// Three numbers in this file were wrong when this was written, all about
    /// the same pair and none checkable by reading: the ink on the old accent
    /// was written as 5.9 in the module note and 6.0 further down, and the
    /// alternative it was compared against — "only 3.1:1" — was
    /// [`Color32::WHITE`]'s ratio, not cream's. Pin the ratios the file
    /// commits to in writing (the dark theme's, which the prose quotes), so
    /// editing a colour and not the paragraph beside it is a failing test
    /// rather than a stale comment.
    #[test]
    fn the_documented_ratios_are_the_measured_ones() {
        let c = &DARK;
        for (claim, fg, bg, stated) in [
            ("text on surface", c.text, c.surface, 14.3),
            ("text_dim on surface", c.text_dim, c.surface, 9.4),
            ("accent_text on surface", c.accent_text, c.surface, 17.6),
            ("accent on surface", c.accent, c.surface, 3.9),
            ("on_accent on accent", c.on_accent, c.accent, 4.5),
            ("cream on accent_muted", c.cream, c.accent_muted, 10.5),
            ("line on surface", c.line, c.surface, 3.8),
            ("accent_lift label", c.on_accent, c.accent_lift, 5.1),
            ("accent_ring label", c.on_accent, c.accent_ring, 5.8),
            (
                "on_accent on accent_pressed",
                c.on_accent,
                c.accent_pressed,
                5.8,
            ),
        ] {
            // Compare at the precision the prose actually claims: the docs say
            // "11.9:1", so the test that keeps them honest is whether the
            // measurement *rounds* to that, not whether it equals it.
            let got = contrast(fg, bg);
            assert!(
                (got * 10.0).round() / 10.0 == stated,
                "{claim}: the docs say {stated:.1}:1, the colours measure {got:.2}:1"
            );
        }
    }

    /// egui derives three app-wide text colours from the widget table rather
    /// than exposing them directly — plain text from `noninteractive`, weak text
    /// from `weak_text_color`, and **`RichText::strong()` from `widgets.active`**.
    /// That last one is the trap: `active` is also the pressed-button state, so
    /// picking its `fg_stroke` to suit the fill silently recolours every bold
    /// value in the app. Assert the three land where the palette says, in both
    /// themes.
    #[test]
    fn derived_text_colours_come_from_the_palette() {
        for (theme, c) in BOTH {
            let v = visuals_of(c);
            assert_eq!(v.text_color(), c.text, "{theme}");
            assert_eq!(v.weak_text_color(), c.text_dim, "{theme}");
            assert_eq!(v.strong_text_color(), c.cream, "{theme}");
            assert_eq!(v.dark_mode, c.dark, "{theme}");
        }
    }

    /// The dark palette is the look the person signed off, value for value:
    /// the light theme was added beside it and moved none of it. The accent
    /// plates went a deeper teal with white ink on 2026-11-10, at the
    /// person's word; anything else that moves here should be asked for too.
    #[test]
    fn the_dark_palette_is_the_look_it_was() {
        let rgb = |c: Color32| (c.r(), c.g(), c.b(), c.a());
        assert_eq!(rgb(DARK.ink), (0x0F, 0x14, 0x16, 255));
        assert_eq!(rgb(DARK.surface), (0x14, 0x1A, 0x1C, 255));
        assert_eq!(rgb(DARK.surface_raised), (0x1F, 0x28, 0x29, 255));
        assert_eq!(rgb(DARK.surface_hover), (0x2A, 0x35, 0x36, 255));
        assert_eq!(rgb(DARK.line), (0x63, 0x79, 0x7C, 255));
        assert_eq!(rgb(DARK.accent), (0x2B, 0x83, 0x7E, 255));
        assert_eq!(rgb(DARK.accent_ring), (0x24, 0x70, 0x6C, 255));
        assert_eq!(rgb(DARK.accent_pressed), (0x24, 0x70, 0x6C, 255));
        assert_eq!(rgb(DARK.accent_muted), (0x26, 0x3F, 0x42, 255));
        assert_eq!(rgb(DARK.cream), (0xF4, 0xF8, 0xF8, 255));
        assert_eq!(rgb(DARK.text), (0xE2, 0xE9, 0xEA, 255));
        assert_eq!(rgb(DARK.text_dim), (0xAA, 0xC2, 0xC6, 255));
        assert_eq!(rgb(DARK.warn), (0xD8, 0xC0, 0x7A, 255));
        assert_eq!(rgb(DARK.error), (0xF2, 0xA2, 0x96, 255));
        assert_eq!(rgb(DARK.panel), (0x17, 0x23, 0x24, 255));
        assert_eq!(rgb(DARK.card), (0x16, 0x1F, 0x21, 255));
        assert_eq!(rgb(DARK.panel_rim), (0x2A, 0x35, 0x36, 255));
        assert_eq!(
            DARK.on_status, DARK.ink,
            "a status plate is labelled in ink"
        );
        for white in [DARK.on_accent, DARK.accent_bright, DARK.accent_text] {
            assert_eq!(white, Color32::WHITE);
        }
        assert_eq!(rgb(DARK.accent_lift), (0x27, 0x7A, 0x75, 255));
        assert_eq!(rgb(DARK.accent_word), (0xA8, 0xDC, 0xD8, 255));
        assert_eq!((DARK.overlay, DARK.glass), (DARK.panel, DARK.panel));
        assert_eq!(DARK.overlay_line, DARK.panel_rim);
        assert_eq!(DARK.shadow, 1.0);
    }

    /// The wordmark is the app's name split in two, and nothing else. It is
    /// written in three places — here, `index.html`'s loader and the account
    /// GUI's header — so the halves are exactly where a typo would ship as a
    /// misspelt product name that still looks deliberate.
    #[test]
    fn the_wordmark_halves_spell_the_app() {
        assert_eq!(format!("{WORDMARK_HEAD}{WORDMARK_TAIL}"), "cartopolis");
    }

    /// The pin is one `convex_polygon`, and epaint fills a polygon that is not
    /// convex wrongly rather than refusing it — a teardrop regenerated with a
    /// dent would paint as a wedge with nothing to say why.
    #[test]
    fn the_pin_outline_is_convex() {
        let o = &mark::PIN_OUTLINE;
        let mut sign = 0.0_f32;
        for i in 0..o.len() {
            let (a, b, c) = (o[i], o[(i + 1) % o.len()], o[(i + 2) % o.len()]);
            let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
            if cross.abs() < 1e-3 {
                continue;
            }
            assert!(
                sign == 0.0 || sign == cross.signum(),
                "the pin outline turns the other way at point {i}"
            );
            sign = cross.signum();
        }
    }

    /// Nothing in the plan reaches outside the tile or into the pin's halo. The
    /// mark is painted with no clip of any kind — a `Painter` at the sign-in
    /// card's 22 pt will happily draw a street across the card's own text — so
    /// the generator's cut is the only clip there is.
    #[test]
    fn the_plan_stays_inside_the_tile_and_clear_of_the_pin() {
        let square = 2.0 * mark::UNIT;
        let o = &mark::PIN_OUTLINE;
        let to_pin = |p: [f32; 2]| {
            (0..o.len())
                .map(|i| {
                    let (a, b) = (o[i], o[(i + 1) % o.len()]);
                    let d = [b[0] - a[0], b[1] - a[1]];
                    let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1])
                        / (d[0] * d[0] + d[1] * d[1]))
                        .clamp(0.0, 1.0);
                    (p[0] - a[0] - t * d[0]).hypot(p[1] - a[1] - t * d[1])
                })
                .fold(f32::INFINITY, f32::min)
        };
        assert!(!mark::LINES.is_empty(), "the plan is empty");
        for (i, line) in mark::LINES.iter().enumerate() {
            for &p in line.points {
                assert!(
                    (0.0..=square).contains(&p[0]) && (0.0..=square).contains(&p[1]),
                    "line {i} reaches ({}, {}), outside the tile",
                    p[0],
                    p[1]
                );
                // One unit of slack for the generator's rounding to whole units.
                assert!(
                    to_pin(p) >= mark::PIN_HALO + line.width / 2.0 - 1.0,
                    "line {i} runs into the pin's halo at ({}, {})",
                    p[0],
                    p[1]
                );
            }
        }
    }

    /// The brand family must be bound before anything paints a heading —
    /// `install` exists to guarantee that, and an unbound family is a panic deep
    /// in epaint rather than a missing-font fallback.
    #[test]
    fn install_binds_the_brand_family_for_the_first_frame() {
        let ctx = egui::Context::default();
        install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let _ = ctx.run_ui_unpainted(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                ui.heading("Places");
            },
        );
    }

    /// The two brand primitives paint, on the first pass, into a bare context.
    ///
    /// Cheap, and it covers the one way this could fail at runtime rather than at
    /// compile time: [`paint_wordmark`] lays its galleys out in
    /// [`brand_family`], and an unbound family is a panic deep in epaint, not a
    /// fallback. The splash paints on frame one — there is no second chance to
    /// find that out.
    #[test]
    fn the_mark_and_the_wordmark_paint_on_the_first_pass() {
        let ctx = egui::Context::default();
        install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let out = ctx.run_ui_unpainted(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                let p = ui.painter();
                paint_mark(p, egui::pos2(200.0, 100.0), 60.0, 1.0);
                paint_wordmark(p, egui::pos2(200.0, 220.0), SPLASH_WORDMARK, 1.0);
            },
        );
        // The tile, the street plan, the pin and its dot, and the wordmark's
        // two galleys.
        assert!(
            out.shapes.len() >= 30,
            "the brand painted only {} shapes",
            out.shapes.len()
        );
    }

    /// Every `RightPanel`, walked by index rather than hand-listed.
    ///
    /// The list under the test used to be written out by hand, and `Script` was
    /// added to the enum without being added to it — so the newest panel was the
    /// one panel whose icon nothing checked. The round trip below is what stops
    /// that repeating: the second `match` has no wildcard, so a new variant does
    /// not compile until it is given an index, and giving it an index sends you
    /// straight to the table above it.
    fn panel_at(i: usize) -> Option<crate::resources::RightPanel> {
        use crate::resources::RightPanel as P;
        let panel = match i {
            0 => P::None,
            1 => P::Search,
            2 => P::Layers,
            3 => P::Places,
            4 => P::Route,
            5 => P::Chat,
            6 => P::Appearance,
            7 => P::Script,
            8 => P::Models,
            9 => P::Settings,
            10 => P::About,
            11 => P::Admin,
            _ => return None,
        };
        let back = match panel {
            P::None => 0,
            P::Search => 1,
            P::Layers => 2,
            P::Places => 3,
            P::Route => 4,
            P::Chat => 5,
            P::Appearance => 6,
            P::Script => 7,
            P::Models => 8,
            P::Settings => 9,
            P::About => 10,
            P::Admin => 11,
        };
        assert_eq!(back, i, "panel_at's two tables disagree at {i}");
        Some(panel)
    }

    /// No panel may wear a glyph the font cannot draw.
    ///
    /// Kept after the icon font landed, and narrower than
    /// `icons::tests::every_icon_has_a_glyph`: that one checks the table, this
    /// one checks that each panel is actually *wired* to a row of it. A panel
    /// added with a hand-typed glyph — which is how `🧭`, `🤖` and `🧍` shipped as
    /// empty boxes, twice over once the header started repeating the dock's icon
    /// — fails here rather than on somebody's screen.
    #[test]
    fn every_panel_icon_resolves_to_a_glyph_the_font_has() {
        let ctx = egui::Context::default();
        install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let _ = ctx.run_ui_unpainted(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                let mut seen = 0;
                while let Some(panel) = panel_at(seen) {
                    seen += 1;
                    let icon = panel.icon();
                    let font = TextStyle::Body.resolve(ui.style());
                    assert!(
                        ui.ctx().fonts_mut(|f| f.has_glyphs(&font, icon)),
                        "{}: wears {icon:?}, which the font cannot draw",
                        panel.label(),
                    );
                }
                assert!(seen > 1, "the panel walk found nothing to check");
            },
        );
    }

    // ─── The source-walking tofu test ─────────────────────────────────────────

    /// One string literal, located.
    struct Literal {
        file: String,
        line: usize,
        text: String,
    }

    /// Every string literal in a file, with comments, `#[cfg(test)]` modules and
    /// preference lists (see [`literals_of`]) already accounted for.
    ///
    /// Deliberately a 60-line lexer rather than a regex: a regex over Rust source
    /// cannot tell `"//"` inside a string from a comment, and getting *that*
    /// backwards is how a scanner reports glyphs that no one ever paints.
    ///
    /// Two known limitations, both checked against the tree: a `'"'` char literal
    /// would desynchronise the scan (there is none, and one would be caught by the
    /// resulting nonsense rather than passing quietly), and `\u{…}` escapes are
    /// read as their literal characters, so a glyph spelled as an escape is not
    /// checked.
    fn literals_of(file: &str, src: &str) -> Vec<Literal> {
        // The suite's convention is one trailing `#[cfg(test)]\nmod tests`; a
        // `#[cfg(test)] fn` helper mid-file (driver.rs has one) must *not* cut the
        // scan, or 900 lines of the UI that draws the navigation chrome go
        // unchecked.
        let src: &str = match src.find("#[cfg(test)]\nmod tests") {
            Some(cut) => &src[..cut],
            None => src,
        };
        let c: Vec<char> = src.chars().collect();
        let n = c.len();
        // (line, start, end, text) per literal, in source order.
        let mut lits: Vec<(usize, usize, usize, String)> = Vec::new();
        let mut i = 0;
        let mut line = 1;
        let two = |i: usize, a: char, b: char| i + 1 < n && c[i] == a && c[i + 1] == b;
        while i < n {
            if c[i] == '\n' {
                line += 1;
                i += 1;
                continue;
            }
            if two(i, '/', '/') {
                while i < n && c[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            if two(i, '/', '*') {
                let mut depth = 1;
                i += 2;
                while i < n && depth > 0 {
                    if c[i] == '\n' {
                        line += 1;
                    }
                    if two(i, '/', '*') {
                        depth += 1;
                        i += 2;
                    } else if two(i, '*', '/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            // Raw string: r"…", r#"…"#, r##"…"##. The look-behind keeps an
            // identifier that merely *ends* in `r` from opening one.
            let word_before = i > 0 && (c[i - 1].is_alphanumeric() || c[i - 1] == '_');
            if c[i] == 'r' && !word_before && i + 1 < n && (c[i + 1] == '"' || c[i + 1] == '#') {
                let start_line = line;
                let mut j = i + 1;
                let mut hashes = 0;
                while j < n && c[j] == '#' {
                    hashes += 1;
                    j += 1;
                }
                if j < n && c[j] == '"' {
                    let start = i;
                    let body = j + 1;
                    let mut k = body;
                    let closes = |k: usize| -> bool {
                        c[k] == '"' && (1..=hashes).all(|h| k + h < n && c[k + h] == '#')
                    };
                    while k < n && !closes(k) {
                        if c[k] == '\n' {
                            line += 1;
                        }
                        k += 1;
                    }
                    let text: String = c[body..k.min(n)].iter().collect();
                    let end = (k + 1 + hashes).min(n);
                    lits.push((start_line, start, end, text));
                    i = end;
                    continue;
                }
            }
            if c[i] == '"' {
                let start = i;
                let start_line = line;
                let mut j = i + 1;
                let mut text = String::new();
                while j < n && c[j] != '"' {
                    if c[j] == '\\' && j + 1 < n {
                        text.push(c[j + 1]);
                        j += 2;
                        continue;
                    }
                    if c[j] == '\n' {
                        line += 1;
                    }
                    text.push(c[j]);
                    j += 1;
                }
                lits.push((start_line, start, (j + 1).min(n), text));
                i = j + 1;
                continue;
            }
            i += 1;
        }

        // A bracketed run of nothing but string literals is a preference list —
        // the shape `first_drawable` took before the icon font retired it, and
        // one anybody may write again for a non-icon glyph. Its whole point is
        // that the earlier entries may be missing and only the last has to
        // resolve, so the scan skips all but the last.
        let mut fallback_only = vec![false; lits.len()];
        let mut k = 0;
        while k < lits.len() {
            let mut j = lits[k].1;
            while j > 0 && c[j - 1].is_whitespace() {
                j -= 1;
            }
            if j > 0 && c[j - 1] == '[' {
                let mut last = k;
                loop {
                    let mut p = lits[last].2;
                    while p < n && (c[p].is_whitespace() || c[p] == ',') {
                        p += 1;
                    }
                    if last + 1 < lits.len() && lits[last + 1].1 == p {
                        last += 1;
                    } else {
                        break;
                    }
                }
                if last > k {
                    for entry in fallback_only.iter_mut().take(last).skip(k) {
                        *entry = true;
                    }
                    k = last + 1;
                    continue;
                }
            }
            k += 1;
        }

        lits.into_iter()
            .zip(fallback_only)
            .filter(|(_, skip)| !*skip)
            .map(|((line, _, _, text), _)| Literal {
                file: file.to_string(),
                line,
                text,
            })
            .collect()
    }

    /// A literal longer than this is prose, not a label: an LLM tool schema, a
    /// `tracing` line, an assertion message. None of them is ever laid out by
    /// egui, and none of them can be told apart from a painted label by anything
    /// a lexer can see — so the scan stops at the length where "this is a caption"
    /// stops being true.
    const MAX_LABEL_CHARS: usize = 24;

    /// Painted glyphs that the font cannot draw **today**, and whose files belong
    /// to someone else this week. Each entry is a bug, not an exemption.
    ///
    /// The test asserts every entry is still *hit*, so fixing one turns this list
    /// red until the entry is deleted with it — an allowlist that cannot quietly
    /// outlive its reason.
    ///
    /// **Currently empty, and that is the goal state.** All five entries it was
    /// created with were fixed the same day it was written; the const stays so the
    /// next one has an obvious place to be recorded rather than silenced.
    const KNOWN_TOFU: &[(&str, char, &str)] = &[];

    /// Walk every `.rs` file under the crate and collect them.
    fn source_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                source_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// **Nothing the app paints may be a glyph the font lacks.**
    ///
    /// The icons this rule was written for are gone from the source: they live
    /// in [`crate::systems::icons`] now, as constants backed by a font the app
    /// ships, and `icons::tests::every_icon_has_a_glyph` is what checks *those*.
    /// This test still matters for everything else — a `≈` in a readout, a `°`,
    /// the next symbol somebody types straight into a label — which is exactly
    /// the case that shipped broken twice: the HUD's `⧉` copy button and the
    /// furniture prompt's `🪑`, neither visible in a review, a `cargo check` or a
    /// screenshot nobody happens to take of that corner.
    ///
    /// So the test does what a reviewer cannot: it reads the source, takes every
    /// short string literal that is not part of a preference list, and asks the
    /// *loaded font stack* whether each non-Latin-1 character in it has a glyph.
    /// Latin-1 is exempt because Ubuntu-Light covers it and a `°` or a `·` is not
    /// a guess anyone is making.
    /// The name is painted in one orange on both halves of the startup screen.
    ///
    /// `systems::splash` and `index.html`'s loader are one screen handed over
    /// mid-load, and the module doc has said "change one and change the other"
    /// since they were built. They drifted anyway, silently and in the way that
    /// rule cannot catch: the Rust side used `ACCENT_TEXT` (`#FFA470`) and the CSS
    /// used `#FF9457`, two different lightened accents, neither of them the plain
    /// `#FD6012` of the mark's ring right above them and the progress bar right
    /// below. Nobody sees the handover as a colour change — the wordmark simply
    /// reads pink on a screen whose every other orange is the brand's.
    ///
    /// Reading the real `index.html` rather than restating the hex here: a
    /// constant copied into a test is a second place to drift, and this is a test
    /// about two places already having done that.
    #[test]
    fn the_wordmark_is_one_orange_on_both_splashes() {
        assert_eq!(
            WORDMARK_TAIL_COLOR, BRAND,
            "the wordmark tail is the brand orange — not the chrome's accent, \
             which is the teal"
        );
        let css = format!(
            "#{:02x}{:02x}{:02x}",
            WORDMARK_TAIL_COLOR.r(),
            WORDMARK_TAIL_COLOR.g(),
            WORDMARK_TAIL_COLOR.b()
        );
        let loader = include_str!("../../../index.html");
        let rule = loader
            .split("#wordmark b {")
            .nth(1)
            .expect("index.html still styles the wordmark tail")
            .split('}')
            .next()
            .expect("`#wordmark b` rule is closed");
        assert!(
            rule.to_ascii_lowercase().contains(&css),
            "index.html paints the wordmark tail {rule:?}, but the app paints it {css}"
        );
    }

    #[test]
    fn the_emoji_people_type_in_chat_are_drawn() {
        // Chat text is whatever people type, so no ladder can stand in for a
        // missing glyph there. 🙂 and 🕺 drew as `◻` on the phone until the
        // current Noto Emoji joined the stack (`EMOJI_TTF`).
        let ctx = egui::Context::default();
        install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let _ = ctx.run_ui_unpainted(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                let font = TextStyle::Body.resolve(ui.style());
                for ch in ['🙂', '🕺', '🤖', '🛒', '👍', '🥳', '🫠'] {
                    let width = ui.ctx().fonts_mut(|f| f.glyph_width(&font, ch));
                    assert!(width > 0.0, "{ch} (U+{:X}) draws as a box", ch as u32);
                }
            },
        );
    }

    #[test]
    fn no_painted_glyph_is_tofu() {
        // **Both crates that put a glyph on screen, not just this one.**
        // `cartopolis_geo` used to own two hard-coded icon tables —
        // `PoiCategory::glyph` and `TravelMode::icon` — whose output was painted
        // straight into the interact prompt and the route panel. Scanning only
        // this crate's `src` let `🛍` (Shop) and `🛏` (Lodging) ship as empty
        // boxes for as long as they existed: the rule was right and its reach
        // stopped at a crate boundary. Those tables moved to `systems::icons`
        // with the icon font, but the reach is kept — a label written over there
        // is painted over here just the same, and `cartopolis_geo` cannot depend
        // on egui, so it cannot host this test itself.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let roots = [manifest.join("src"), manifest.join("../geo/src")];
        let mut files = Vec::new();
        for root in &roots {
            source_files(root, &mut files);
        }
        assert!(
            files.len() > 20,
            "found only {} source files under {:?} — the walk is broken, not the tree",
            files.len(),
            roots
        );

        let mut literals = Vec::new();
        for path in &files {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            literals.extend(literals_of(&name, &src));
        }

        let ctx = egui::Context::default();
        install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let _ = ctx.run_ui_unpainted(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                let font = TextStyle::Body.resolve(ui.style());
                // The width test below must still see a character nothing
                // draws, or it passes everything.
                assert_eq!(
                    ui.ctx().fonts_mut(|f| f.glyph_width(&font, '\u{10FFFD}')),
                    0.0,
                    "a character no font has must measure 0 wide"
                );
                let mut missing: Vec<(String, usize, char)> = Vec::new();
                for lit in &literals {
                    if lit.text.chars().count() > MAX_LABEL_CHARS {
                        continue;
                    }
                    for ch in lit.text.chars() {
                        if (ch as u32) <= 0xFF {
                            continue;
                        }
                        let glyph = ch.to_string();
                        // `has_glyphs` answers "not the replacement face", and
                        // the face epaint draws `◻` from is Noto Emoji — so every
                        // emoji, though Noto Emoji draws it, read as missing. A
                        // character no face has measures 0 wide; one the
                        // replacement face really has does not.
                        let drawable = ui.ctx().fonts_mut(|f| {
                            f.has_glyphs(&font, &glyph) || f.glyph_width(&font, ch) > 0.0
                        });
                        if !drawable {
                            missing.push((lit.file.clone(), lit.line, ch));
                        }
                    }
                }

                let excused = |file: &str, ch: char| {
                    KNOWN_TOFU.iter().any(|(f, c, _)| *f == file && *c == ch)
                };
                let unexcused: Vec<String> = missing
                    .iter()
                    .filter(|(file, _, ch)| !excused(file.as_str(), *ch))
                    .map(|(file, line, ch)| {
                        format!("{file}:{line} draws U+{:04X} {ch:?}", *ch as u32)
                    })
                    .collect();
                assert!(
                    unexcused.is_empty(),
                    "the font cannot draw these, so they paint as empty boxes — if it is \
                     an icon, take it from `systems::icons`; otherwise use something \
                     Latin-1, which every font here covers:\n  {}",
                    unexcused.join("\n  ")
                );

                // …and the other direction: an entry that no longer matches
                // anything has been fixed, and a stale excuse is how the next one
                // gets waved through.
                for &(file, ch, why) in KNOWN_TOFU {
                    assert!(
                        missing
                            .iter()
                            .any(|(f, _, c)| f.as_str() == file && *c == ch),
                        "KNOWN_TOFU still lists {ch:?} in {file} ({why}), but the scan no \
                         longer finds it — delete the entry",
                    );
                }
            },
        );
    }

    // ─── The scales ───────────────────────────────────────────────────────────

    /// The type scale is a *scale*: strictly increasing, with no two entries close
    /// enough to be indistinguishable on screen. Two sizes half a point apart are
    /// not a choice, they are the drift this replaced (the app shipped 9.5, 10,
    /// 10.5, 11 and 11.5 as if they were five different intentions).
    #[test]
    fn the_type_scale_is_a_scale() {
        let scale = [
            ("tiny", SIZE_TINY),
            ("chip", SIZE_CHIP),
            ("sub", SIZE_SUB),
            ("body", SIZE_BODY),
            ("value", SIZE_VALUE),
            ("title", SIZE_TITLE),
            ("heading", SIZE_HEADING),
            ("panel title", SIZE_PANEL_TITLE),
        ];
        for pair in scale.windows(2) {
            let ((a, lo), (b, hi)) = (pair[0], pair[1]);
            assert!(
                hi - lo >= 1.0,
                "{a} ({lo}) and {b} ({hi}) are the same size"
            );
        }
        // The style's own entries must come off the scale, or the constants
        // describe a UI the app is not painting.
        let s = style_for(false);
        assert_eq!(s.text_styles[&TextStyle::Body].size, SIZE_BODY);
        assert_eq!(s.text_styles[&TextStyle::Small].size, SIZE_CHIP);
        assert_eq!(s.text_styles[&TextStyle::Heading].size, SIZE_HEADING);
    }

    /// The radius scale, same argument: four values, each visibly a step from the
    /// next, and [`RADIUS_PILL`] beyond any box this app draws.
    #[test]
    fn the_radius_scale_is_a_scale() {
        assert!(RADIUS_SM < RADIUS && RADIUS < RADIUS_LG);
        assert!(RADIUS_LG < RADIUS_PILL);
        // The widgets take the default one — this is the value a caller who
        // needed it used to read back off the widget table instead.
        assert_eq!(
            visuals().widgets.inactive.corner_radius,
            CornerRadius::same(RADIUS)
        );
    }

    /// The touch tier exists to clear [`MIN_TOUCH_TARGET_DP`], and the arithmetic
    /// that says it does is the only check available in a container with no
    /// glass. Both platforms that reach the tier are covered, because they get
    /// there by different routes: Android magnifies the UI (`ui_scale` 1.5) while
    /// the web magnifies the *canvas* (`index.html`'s stage transform).
    #[test]
    fn touch_sizes_clear_the_minimum_target() {
        // Native Android: ui_scale 1.5, no stage transform.
        let android = dp_per_point(1.5, 1.5, false);
        // Web phone: ui_scale 1.0, and a Fairphone 4's 2.75× DPR through the
        // stage transform.
        let web = dp_per_point(1.0, 2.75, true);
        // A desktop — including one that forced the on-screen keyboard on with
        // `?osk` — magnifies nothing, so a point is a dp.
        assert!((dp_per_point(1.0, 1.0, false) - 1.0).abs() < 1e-6);
        // And the keyboard's own floor, which is the tightest consumer of the
        // pair: 48 dp at `ui_scale` 1.5 is a 32 pt key.
        assert!((min_touch_target_pts(android) - 32.0).abs() < 0.01);
        let web_key = min_touch_target_pts(web);
        assert!(web_key > 25.0 && web_key < 27.0, "web key {web_key} pt");
        for (name, dp_per_pt) in [("android", android), ("web", web)] {
            let row_dp = TOUCH_ROW_PTS * dp_per_pt;
            assert!(
                row_dp >= MIN_TOUCH_TARGET_DP,
                "{name}: a touch row is {row_dp:.0} dp, under the {MIN_TOUCH_TARGET_DP:.0} dp \
                 guideline"
            );
            assert!(
                ROUND_BUTTON_SIZE * dp_per_pt >= MIN_TOUCH_TARGET_DP,
                "{name}: a dock disc is under the guideline"
            );
            // And the conversion the keyboard needs agrees with the row.
            assert!(min_touch_target_pts(dp_per_pt) <= TOUCH_ROW_PTS + 0.01);
            // …and why [`action`] refuses `small()` under a finger. egui's small
            // button zeroes the vertical padding and drops to the `Small` text
            // style, so its height *is* the glyph — a third of the row it sits
            // in, on the controls that exist to save you typing an address on a
            // phone.
            let small_dp = style_for(true).text_styles[&TextStyle::Small].size * dp_per_pt;
            assert!(
                small_dp < MIN_TOUCH_TARGET_DP,
                "{name}: a small button is {small_dp:.0} dp, so `actions_are_small` \
                 has nothing left to protect against"
            );
        }
        // The pointer tier is deliberately *not* held to it — a mouse is not a
        // finger, and 24 pt rows are what makes a panel of layer toggles readable.
        let touch = style_for(true);
        let pointer = style_for(false);
        assert!(touch.spacing.interact_size.y > pointer.spacing.interact_size.y);
        assert_eq!(touch.spacing.interact_size.y, TOUCH_ROW_PTS);
        // Type does not grow with the targets: the phone's UI is already sharp,
        // and scaling the text too is what pushes the viewport under
        // `responsive_right_panel`'s 240 pt floor.
        assert_eq!(
            touch.text_styles[&TextStyle::Body].size,
            pointer.text_styles[&TextStyle::Body].size
        );
    }
}

/// Looking at the mark, rather than reasoning about it.
///
/// `cargo test -p cartopolis write_mark_preview -- --ignored --nocapture` writes
/// `/tmp/mark_<radius>.png` at the four sizes the mark is drawn at. It
/// rasterises the **tessellated** output — the triangles egui would hand the
/// GPU — so it shows the real thing including the minimum stroke widths and the
/// detail cutoff, which is what a hand-written SVG of the same table would not.
///
/// It exists because the mark is a reduction of `assets/icon/cartopolis.svg`
/// (see [`mark`]) and a reduction can only be judged beside its source. Ignored
/// because it writes files and asserts nothing.
#[cfg(test)]
mod mark_preview {
    use super::*;
    use crate::systems::egui_manager::UnpaintedPasses as _;

    /// Sizes: the splash, a mid size, the sign-in card, and one below
    /// [`mark::DETAIL_RADIUS`] to see what the cutoff leaves.
    const PREVIEW_RADII: [f32; 4] = [84.0, 40.0, 22.0, 12.0];

    #[test]
    #[ignore = "eyeball hatch: it writes PNGs to look at, it asserts nothing"]
    fn write_mark_preview() {
        let ppp = 2.0_f32;
        for radius in PREVIEW_RADII {
            let pad = 8.0;
            let side = radius * 2.0 + pad * 2.0;
            let ctx = egui::Context::default();
            install(&ctx);
            ctx.set_pixels_per_point(ppp);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(side, side));
            let out = ctx.run_ui_unpainted(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ui| {
                    let p = ui.painter();
                    p.rect_filled(screen, 0.0, DARK.surface);
                    paint_mark(p, screen.center(), radius, 1.0);
                },
            );
            let prims = ctx.tessellate(out.shapes, ppp);
            let w = (side * ppp) as usize;
            let mut buf = vec![[0.0_f32; 4]; w * w];
            for prim in prims {
                let egui::epaint::Primitive::Mesh(mesh) = prim.primitive else {
                    continue;
                };
                for tri in mesh.indices.chunks(3) {
                    let v: Vec<_> = tri.iter().map(|&i| mesh.vertices[i as usize]).collect();
                    let (p0, p1, p2) = (v[0].pos * ppp, v[1].pos * ppp, v[2].pos * ppp);
                    let area = (p1.x - p0.x) * (p2.y - p0.y) - (p2.x - p0.x) * (p1.y - p0.y);
                    if area.abs() < 1e-6 {
                        continue;
                    }
                    let minx = p0.x.min(p1.x).min(p2.x).floor().max(0.0) as usize;
                    let maxx = (p0.x.max(p1.x).max(p2.x).ceil() as usize).min(w - 1);
                    let miny = p0.y.min(p1.y).min(p2.y).floor().max(0.0) as usize;
                    let maxy = (p0.y.max(p1.y).max(p2.y).ceil() as usize).min(w - 1);
                    for y in miny..=maxy {
                        for x in minx..=maxx {
                            let px = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                            let w0 = ((p1.x - px.x) * (p2.y - px.y)
                                - (p2.x - px.x) * (p1.y - px.y))
                                / area;
                            let w1 = ((p2.x - px.x) * (p0.y - px.y)
                                - (p0.x - px.x) * (p2.y - px.y))
                                / area;
                            let w2 = 1.0 - w0 - w1;
                            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                                continue;
                            }
                            let mut src = [0.0_f32; 4];
                            for (k, vert) in v.iter().enumerate() {
                                let bw = [w0, w1, w2][k];
                                let c = vert.color;
                                // Vertex colours are premultiplied sRGB bytes.
                                src[0] += c.r() as f32 * bw;
                                src[1] += c.g() as f32 * bw;
                                src[2] += c.b() as f32 * bw;
                                src[3] += c.a() as f32 * bw;
                            }
                            let dst = &mut buf[y * w + x];
                            let inv = 1.0 - src[3] / 255.0;
                            for k in 0..4 {
                                dst[k] = src[k] + dst[k] * inv;
                            }
                        }
                    }
                }
            }
            // Downsample by `ppp` so the file is the size the mark is drawn at.
            let out_w = w / ppp as usize;
            let mut img = image::RgbaImage::new(out_w as u32, out_w as u32);
            let n = ppp as usize;
            for y in 0..out_w {
                for x in 0..out_w {
                    let mut acc = [0.0_f32; 4];
                    for dy in 0..n {
                        for dx in 0..n {
                            let s = buf[(y * n + dy) * w + x * n + dx];
                            for k in 0..4 {
                                acc[k] += s[k];
                            }
                        }
                    }
                    let d = (n * n) as f32;
                    img.put_pixel(
                        x as u32,
                        y as u32,
                        image::Rgba([
                            (acc[0] / d) as u8,
                            (acc[1] / d) as u8,
                            (acc[2] / d) as u8,
                            255,
                        ]),
                    );
                }
            }
            let path = format!("/tmp/mark_{}.png", radius as i32);
            img.save(&path).unwrap();
            println!("wrote {path} ({out_w}px)");
        }
    }
}
