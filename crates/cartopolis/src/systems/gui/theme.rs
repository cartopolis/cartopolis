//! The Cartopolis look: one palette, one `egui::Style`, one place to change them.
//!
//! A graphite ramp from `#13191B` up to `#283236` carries the chrome, a pale
//! `#C7D3D8` carries every live thing, and `#F3F7F9` is the off-white. The
//! chrome is **monochrome**: one hue, 197° — a blue-teal — at 6–14 % saturation
//! for every surface, rim, fill and text tone in the ramp. Read the numbers and
//! it is a cool grey; put it beside a true grey and it is glass. The only other
//! colours in the palette are the two that are *statements* rather than
//! decoration ([`WARN`] and [`ERROR`]) and the brand orange the splash and the
//! app mark keep for themselves ([`BRAND`]).
//!
//! **One hue, not none.** The ramp was neutral for half a day and it was flat:
//! a colourless panel over a coloured map reads as a dialog pasted onto a
//! photograph, and glass has never been colourless — it is the one material
//! whose whole character is the faint cast it gives what you see through it.
//! The tint is deliberately below the threshold where anyone would call the UI
//! blue. It is applied at **constant lightness** (hue rotated in HSL, saturation
//! raised ~30 %, `l` untouched), so every ratio the tests pin moved by at most
//! 0.5 and none of the tonal structure the widget table depends on changed.
//!
//! **The chrome went monochrome on 2026-09-10, and the argument is the same one
//! the desaturation before it made, taken to its end.** The ramp was a slate
//! blue with an orange accent on top — a *branded* UI, in the sense that the
//! chrome carried the logo's colours rather than the map's content. Over a
//! basemap that is itself full of hue (green parks, blue water, beige paper,
//! and, on the aerial layer, a photograph) two saturated families competed for
//! the same eye: the orange never stopped meaning "look here", so it stopped
//! meaning anything. Removing the hue removes the competition, and what is left
//! to say "this is the live thing" is **lightness**, which the map cannot take
//! away — the accent is now the brightest thing on a dark plate, and an engaged
//! control inverts to a light plate with dark ink.
//!
//! Three consequences worth knowing before editing a value:
//!
//!   - **The state ladder is monotonic in lightness** — rest ([`SURFACE_RAISED`])
//!     → hover ([`SURFACE_HOVER`] plus an [`ACCENT`] rim) → pressed
//!     ([`ACCENT_MUTED`] plus an [`ACCENT_RING`] rim) → engaged (an [`ACCENT`]
//!     plate carrying [`INK`]). Each step is up. Break the order and the UI
//!     loses the only channel it has left to say what state a control is in.
//!   - **Text on the accent is [`INK`], never [`CREAM`]** — the accent is a
//!     *light* tone now, so the pairing that used to be wrong by 2.7:1 is wrong
//!     by 1.4:1 in the other direction. This is what `active.weak_bg_fill` being
//!     [`ACCENT_MUTED`] protects (see [`widgets`]).
//!   - **Two constants deliberately share a value.** [`ACCENT_TEXT`] and
//!     [`ACCENT_BRIGHT`] are both pure white: one is type, one is a stroke, they
//!     answer to different tests, and in a palette with no hue left the top of
//!     both ladders is the same white. Keeping both names is what lets one move
//!     without the other.
//!
//! **The surfaces are glass.** [`GLASS`] — the panel and window fill — is
//! [`SURFACE`] at 95 %, so the map moves behind a panel instead of being cut out
//! by it, and the rims are white at low alpha ([`OVERLAY_LINE`]) rather than a
//! drawn border. There is no backdrop blur: egui has no such pass, and adding
//! one would mean a render-graph node reading the main texture per panel. What
//! carries the effect instead is the translucency, the light top rim, the wider
//! corner radii and a soft, far shadow — and the alpha is the one number that is
//! *not* free to move for looks, because it is what the contrast tests over a
//! bright basemap are measured through.
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
//! [`TEXT`] is 13.5:1 on [`SURFACE`], [`TEXT_DIM`] 8.2:1, [`ACCENT_TEXT`] 17.8:1.
//! Plain [`ACCENT`] is 11.6:1 — it is a *light* tone, so it is comfortable as a
//! fill, a rule or a focus ring on any surface in the ramp. Anything sitting *on*
//! an accent fill is drawn in [`INK`] (12.7:1) rather than [`CREAM`] (1.4:1).
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

use crate::systems::icons;

// ─── Palette ──────────────────────────────────────────────────────────────────

/// Deeper than any of the surface tones: text-edit wells, code blocks, and the
/// boot backdrop. Also the ink on an [`ACCENT`] plate.
pub const INK: Color32 = Color32::from_rgb(0x0A, 0x0E, 0x10);
/// The surface ramp's bottom, and the tone every glass surface is made of.
///
/// Opaque, and used as such by the handful of call sites that need a solid
/// panel colour; the *panel and window fill* is [`GLASS`], which is this at
/// 95 %. Keep the two in step — a solid box beside a glass one that were meant
/// to be the same surface is exactly the drift the shared frames exist to stop.
pub const SURFACE: Color32 = Color32::from_rgb(0x13, 0x19, 0x1B);
/// The ramp's middle. One step up from [`SURFACE`]: buttons at rest,
/// striped rows, anything that should read as sitting *on* a panel.
pub const SURFACE_RAISED: Color32 = Color32::from_rgb(0x1D, 0x24, 0x27);
/// The ramp's top. Hover, and open menus over a panel.
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x28, 0x32, 0x36);
/// Hairlines: separators, widget frames, indent guides.
///
/// **Lightened on 2026-08-14, and the reason is WCAG 1.4.11.** (It was a slate
/// blue then and is a neutral grey now; the argument and the ratio survived the
/// repaint unchanged, which is what the number below is for.)
/// A button at rest is [`SURFACE_RAISED`] on [`SURFACE`], which is 1.24:1 of
/// fill difference — so the hairline is the *only* channel that says "this is a
/// control", and at 1.92:1 it was not saying it. What had been carrying the UI
/// is that a panel is dense enough for position to do the work; a control on its
/// own had no boundary. This measures 3.74:1 on [`SURFACE`] and clears the
/// 3:1 floor for a UI component boundary.
///
/// It is a **solid grey, not white-at-an-alpha**, which the glass rims are: a
/// hairline that has to clear a fixed ratio cannot be a translucent one, because
/// what it composites over changes with the surface under it.
pub const LINE: Color32 = Color32::from_rgb(0x69, 0x75, 0x7A);

/// The live tone. Focus rings, hover outlines, sliders, and the plate an engaged
/// control wears — the brightest thing in the chrome, so it always means "this is
/// the live thing".
///
/// **It is a light tone, and everything about the palette follows from that.**
/// The orange it replaced was a mid tone that could carry [`CREAM`] badly and
/// [`INK`] well; this one carries [`INK`] at 12.7:1 and [`CREAM`] at 1.4:1, i.e.
/// not at all. Anything filled with it is labelled in [`INK`].
pub const ACCENT: Color32 = Color32::from_rgb(0xC7, 0xD3, 0xD8);
/// The accent at the top of its ladder: the text cursor, a text-selection
/// handle, the rim on a *held* control (north-up lock, caps lock).
///
/// Deliberately the same value as [`ACCENT_TEXT`] — see the module note. This
/// one is a **stroke**; that one is type.
pub const ACCENT_BRIGHT: Color32 = Color32::WHITE;

/// The ring around an *accent-filled* widget: the pressed button, the focused
/// control, a dock disc whose panel is open, the panel header's medallion.
///
/// Brighter than [`ACCENT`] on purpose, and it does two jobs at once now: on the
/// dark [`ACCENT_MUTED`] of a pressed button it is the boundary (7.2:1, well over
/// the 3:1 a UI component needs), and on the light [`ACCENT`] plate of an engaged
/// disc it is the lit top rim that makes the plate read as glass rather than as a
/// sticker.
pub const ACCENT_RING: Color32 = Color32::from_rgb(0xE5, 0xEC, 0xEF);
/// The accent, pushed *down* — the fill of an accent-filled button while the
/// pointer is held on it.
///
/// A press has to move the surface in the opposite direction from a hover, or
/// the two states are one state: [`primary_button_sized`] lifts to [`ACCENT_BRIGHT`]
/// under the pointer and sinks to this while held, which is what makes a click
/// on the app's one call-to-action *feel* like a click.
///
/// **The direction survived the repaint; the constraint that bounds it did not.**
/// While the accent was a mid orange this was the *darker* orange and the binding
/// number was [`INK`]'s 4.62:1 against it — how far down it could go before its
/// own label stopped reading. The accent is a light tone now, so [`INK`] on this
/// measures 9.4:1 and is nowhere near binding; what bounds it instead is
/// [`ACCENT`] one step above (1.35:1) and [`ACCENT_BRIGHT`] two steps above
/// (2.06:1) — it has to be a visible step down from a plate that is already pale,
/// without falling out of the light half of the ramp and reading as a *different*
/// control rather than the same one pressed.
pub const ACCENT_PRESSED: Color32 = Color32::from_rgb(0xA8, 0xB7, 0xBD);
/// The top of the *text* ladder — [`TEXT`] → [`CREAM`] → this. Links, speaker
/// names, emphasised values.
///
/// With no hue left in the chrome, emphasis is lightness: this is the one pure
/// white in the palette's type, and it reads as emphasis by being a step above
/// the headings rather than by being a different colour.
pub const ACCENT_TEXT: Color32 = Color32::WHITE;
/// The dark half of the accent: a pressed button, a selected row. Dark enough to
/// carry [`CREAM`] at 8.0:1, light enough to be a clear step above
/// [`SURFACE_HOVER`].
///
/// This is what keeps the ladder safe. `active.weak_bg_fill` carries a label
/// drawn in egui's `strong_text_color`, which is [`CREAM`]; filling it with the
/// light [`ACCENT`] instead would make every pressed button's label vanish.
pub const ACCENT_MUTED: Color32 = Color32::from_rgb(0x3E, 0x4E, 0x55);

/// **The brand orange, and the only saturated colour left in this file.**
///
/// It is not an accent and must never be used as one: its whole scope is the
/// logo — the roofs and the pin of [`paint_mark`], the tail of the wordmark
/// ([`WORDMARK_TAIL_COLOR`]) and, in `index.html`, the loader's mark and progress
/// bar. That is the *brand*, which survived the chrome going monochrome on
/// purpose: a product keeps its name in its own colour, and a splash is the one
/// screen with nothing else on it to compete.
///
/// The separation is the point. While the chrome was orange this was spelled
/// `= ACCENT`, so the logo and every live control in the app were one constant —
/// and repainting the chrome would have repainted the logo with it, silently.
pub const BRAND: Color32 = Color32::from_rgb(0xFD, 0x60, 0x12);

/// Off-white. Headings and the one or two things that must read brightest.
pub const CREAM: Color32 = Color32::from_rgb(0xF3, 0xF7, 0xF9);
/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(0xDB, 0xE2, 0xE5);
/// Secondary text: units, coordinates, hints. Also `index.html`'s muted tone,
/// so the pre-wasm splash and this one agree — **change it in both**.
///
/// Lifted on 2026-08-14, and re-hued with the rest of the ramp on 2026-09-10 at
/// the same lightness. The value before that was measured on
/// [`SURFACE`] alone, and this is the colour egui hands to
/// `weak_text_color`, i.e. the one that lands on every surface in the ramp: it
/// measured 4.40:1 on [`SURFACE_HOVER`] and 4.12:1 inside an [`OVERLAY`] box
/// over bright map imagery. Both now clear 4.5.
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xA5, 0xB2, 0xB7);
/// Warnings — a desaturated gold.
///
/// **One of the two colours the monochrome chrome keeps**, and it keeps them for
/// the same reason road signs are not brand-coloured: caution and fault are
/// *statements about the world*, not decoration, and a grey warning is a warning
/// nobody reads as one. It also picked up the jobs the retired second accent
/// used to do wherever those were really caution — a clipping microphone meter,
/// an unsaved script, a frame that took longer than two 60 Hz frames.
pub const WARN: Color32 = Color32::from_rgb(0xD8, 0xC0, 0x7A);
/// Errors. Warm rather than pure red so it belongs to the same palette.
///
/// Lifted from `#E88B7D` on 2026-08-14, for the reason [`TEXT_DIM`] was: an
/// error is the one string that must be readable wherever it lands, and it
/// measured 3.84:1 on [`SURFACE_HOVER`] and 3.59:1 in an [`OVERLAY`] box over a
/// bright basemap.
pub const ERROR: Color32 = Color32::from_rgb(0xF2, 0xA2, 0x96);

/// The fill of the boxes that float *over* the map (status HUD, stats, driver
/// chrome) — the same 84 % glass a panel is made of, and blurred behind by
/// `systems::glass` in the same way.
///
/// **It shares [`GLASS`]'s alpha rather than having one of its own**, which is a
/// change: the two used to differ on the argument that an overlay box holds a
/// line of readout while a panel holds a form, so the box could afford to be
/// thinner. With a blur behind both, the binding constraint stopped being how
/// much detail shows through and became the contrast floor — and that floor is
/// the same for both, because it is set by the text tones, which are also the
/// same. Two numbers that are only ever equal are one number.
///
/// The history is worth keeping, because it is the trap this constant fell into
/// once already. It was **84 % with no blur** (alpha 214) and "opaque enough" was
/// not true at that value: the failure is invisible from this file, because the
/// background is not in it. The imagery is a map, not a satellite photo — pale
/// beige land on near-white paper — so the composite an overlay lands on is
/// `SURFACE` over *white*, three steps lighter than the panel these colours were
/// picked against. [`TEXT_DIM`] measured 4.12:1 there, [`ACCENT_TEXT`] 4.11:1 and
/// [`ERROR`] 3.59:1. It went to 92 % and now back to 84 %, and the difference
/// between the two visits is that the tones themselves were lifted in between —
/// this alpha is a *measured* floor now rather than an eyeballed one, and
/// `text_clears_every_surface_it_can_land_on` is what keeps it one.
pub const OVERLAY: Color32 = Color32::from_rgba_unmultiplied_const(0x13, 0x19, 0x1B, 215);

/// The panel and window fill: [`SURFACE`] at 84 %.
///
/// **The one thing that makes the chrome read as glass rather than as paint.**
/// A panel used to be opaque, which over a moving map is a rectangle cut out of
/// the world; at 84 % the map is plainly *there* behind it — as a wash of colour
/// and shape rather than as a picture, because `systems::glass` blurs it first.
///
/// **The blur is what bought the transparency, and the order matters.** This was
/// 95 % for as long as there was no blur, and it had to be: at 93 % the roof
/// edges and tree crowns of Groningen read straight through the Settings panel's
/// body copy (measured with `--ui graphics`, 2026-09-10). Reducing detail to a
/// wash is exactly what a blur does, so with one behind it the fill can be a
/// sixth of a stop from transparent and still be read over.
///
/// **84 % is a floor, not a taste.** It is the most transparent value at which
/// every text tone still clears 4.5:1 against the worst background a panel can
/// have — the basemap at its brightest, which is white paper, because the
/// imagery is a map rather than a photograph. [`TEXT_DIM`] measures 5.06:1 there
/// at this alpha and **4.40:1 one step further** (alpha 205), and it is the tone
/// egui hands to every hint and unit in the app.
/// `text_clears_every_surface_it_can_land_on` is where that is checked; it
/// composites through this constant, so lowering it moves every one of those
/// ratios at once. Note what the test cannot know: it measures against the
/// *unblurred* white, which is the honest worst case — a blur cannot make a
/// white page darker.
pub const GLASS: Color32 = Color32::from_rgba_unmultiplied_const(0x13, 0x19, 0x1B, 215);

/// The rim around a glass surface — an [`OVERLAY`] box, a panel, a window.
///
/// **White at low alpha, not a grey.** A drawn border says "this is a box"; a
/// light rim says "this is a lit edge", which is the whole difference between a
/// panel that sits on the scene and one that is stamped over it. Being
/// translucent it takes its colour from whatever it lies on, so it stays a
/// highlight over a dark plate and disappears politely over a bright one — which
/// also means it can never be the thing that identifies a *control*. That is
/// [`LINE`]'s job, and [`LINE`] is solid for exactly this reason.
pub const OVERLAY_LINE: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 56);
/// Backing for the small text chips painted straight onto the map — place-pin
/// labels, the drag ghost, a feature's hover card. Darker and more opaque than
/// [`OVERLAY`] on purpose: these are a few words over an arbitrary tile, so
/// legibility beats letting the map through.
pub const LABEL_CHIP: Color32 = Color32::from_rgba_unmultiplied_const(0x0A, 0x0E, 0x10, 235);
/// Halo behind map text that has no chip to sit on — the street names
/// (`systems::street_labels`), which are painted *along* their street and would
/// black it out under a chip. Pale and only mostly opaque: it has to lift
/// [`LABEL_INK`] off tarmac, grass and water without reading as a second word.
/// It used to be tinted toward the chrome's slate-teal, on the argument that a
/// name should belong to the city around it; with the chrome monochrome there is
/// no tint left to carry, and a **neutral** halo is the better answer anyway —
/// the map underneath supplies all the hue in the frame, and a coloured plate
/// over it was a second opinion about what colour a road is.
pub const LABEL_HALO: Color32 = Color32::from_rgba_unmultiplied_const(0xDB, 0xE6, 0xEA, 205);
/// The stroke inside [`LABEL_HALO`]. A hair lighter than [`INK`] and not the
/// same constant: this one lies on a pale halo over an arbitrary tile, that one
/// is the chrome's text-well fill, and the two are free to move apart.
pub const LABEL_INK: Color32 = Color32::from_rgb(0x0D, 0x13, 0x15);

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
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        if let Some(stack) = fonts.families.get_mut(&family) {
            stack.push(icons::FONT_NAME.to_owned());
        }
    }
    // Prepend to a *copy* of the proportional stack rather than pushing onto it:
    // the brand face wins where it has the glyph, egui's fallbacks cover the rest,
    // and `Proportional` itself is left alone so body text keeps its own face.
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
const ROW_PTS: f32 = 24.0;
/// The same on the touch tier: [`MIN_TOUCH_TARGET_DP`] at the tier's own
/// [`dp_per_point`], which is ≥1.5 on both platforms that reach it (native
/// Android's `phone_ui_scale`, and the web's stage transform). 32 pt is 48 dp at
/// exactly 1.5 and more than that above it — an argument from arithmetic, not a
/// measurement on glass.
pub const TOUCH_ROW_PTS: f32 = 32.0;

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

fn widgets() -> Widgets {
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
        noninteractive: visuals(SURFACE, LINE, TEXT),
        inactive: visuals(SURFACE_RAISED, LINE, TEXT),
        // Hover: the surface lifts a step and the hairline goes bright, so
        // "clickable" is a change you can see rather than a guess.
        hovered: WidgetVisuals {
            expansion: 1.0,
            ..visuals(SURFACE_HOVER, ACCENT, CREAM)
        },
        // Pressed / focused — **one fill, not two**, and that is a simplification
        // the monochrome palette bought rather than a detail lost. It used to be
        // two different oranges, because `bg_fill` (the graphical fill: slider
        // grab, checkbox box) could take the full accent while `weak_bg_fill` (the
        // button fill, which carries a label) could not: [`CREAM`] measured only
        // 2.7:1 on the old accent. The accent is a *light* tone now, so that
        // pairing is 1.4:1 — worse, in the same direction — and the answer is for
        // both to be [`ACCENT_MUTED`], which carries [`CREAM`] at 8.5:1 and looks
        // the same whether a control is a button or a checkbox.
        //
        // `fg_stroke` must stay bright regardless: egui derives
        // `Visuals::strong_text_color` from it, so *every* `RichText::strong()` in
        // the app is this colour. Setting it to ink to suit a light fill would
        // turn every bold value in the UI near-invisible — which is exactly the
        // trap the light accent makes easier to fall into, not harder.
        active: WidgetVisuals {
            expansion: 1.0,
            ..visuals(ACCENT_MUTED, ACCENT_RING, CREAM)
        },
        // An open menu button keeps the hover surface but drops the bright ring —
        // the popup below it is the thing to look at now.
        open: visuals(SURFACE_HOVER, LINE, CREAM),
    }
}

/// The full brand [`egui::Visuals`], built by overriding [`egui::Visuals::dark`]
/// rather than from scratch so the fields this doesn't have an opinion about
/// (notably the dark-mode font colour transfer function) stay at egui's tuned
/// defaults.
pub fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.widgets = widgets();
    v.selection = Selection {
        bg_fill: ACCENT_MUTED,
        stroke: Stroke::new(1.0, CREAM),
    };
    v.hyperlink_color = ACCENT_TEXT;
    v.faint_bg_color = SURFACE_RAISED;
    v.extreme_bg_color = INK;
    v.code_bg_color = INK;
    v.warn_fg_color = WARN;
    v.error_fg_color = ERROR;
    // Pin the weak/disabled tone instead of letting egui derive it by alpha —
    // this is the same grey-blue the two splashes use, and it is measured.
    v.weak_text_color = Some(TEXT_DIM);
    // Disabled is the one tone egui still derived by alpha, and its default 0.5
    // put a disabled label at 4.1:1 on the panel — *dimmer than `TEXT_DIM` hint
    // text at 6.9:1*, so the sentence explaining why a control was unavailable
    // was brighter than the control. It also took a button's frame to 1.1:1
    // against the panel, i.e. gone. 0.65 keeps disabled clearly below the live
    // state without making it a colour nobody can find.
    v.disabled_alpha = 0.65;

    // Glass, not paint — see [`GLASS`]. Every panel, window, menu and popup in
    // the app is one surface, so this is the single place the whole chrome's
    // opacity lives.
    v.panel_fill = GLASS;
    v.window_fill = GLASS;
    // A lit rim rather than a drawn border. It is the same hairline the boxes
    // over the map wear, which is the point: a docked panel and a floating
    // readout are the same material at two sizes.
    v.window_stroke = Stroke::new(1.0, OVERLAY_LINE);
    v.window_corner_radius = CornerRadius::same(RADIUS_LG);
    v.menu_corner_radius = CornerRadius::same(RADIUS);
    // Softer and further than egui's default: these float over a moving map, and
    // a tight hard shadow reads as a sticker rather than a raised surface. With
    // the surfaces translucent the shadow does more work than it used to — it is
    // most of what separates one sheet of glass from the one behind it — so it
    // is thrown further and a little darker.
    v.window_shadow = Shadow {
        offset: [0, 14],
        blur: 36,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    v.popup_shadow = Shadow {
        offset: [0, 8],
        blur: 22,
        spread: 0,
        color: Color32::from_black_alpha(125),
    };

    v.text_cursor.stroke = Stroke::new(2.0, ACCENT_BRIGHT);
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
    s.item_spacing = egui::vec2(8.0, 6.0);
    // The touch tier is not "the same UI, bigger" — the *type* stays where it is
    // (it is already sharp on a phone; see `egui_manager::phone_ui_scale`) and
    // only the parts a finger has to hit grow, to [`MIN_TOUCH_TARGET_DP`]. That
    // is the whole difference between the two tiers.
    s.button_padding = if touch {
        egui::vec2(11.0, 8.0)
    } else {
        egui::vec2(9.0, 4.0)
    };
    // 24 pt of guaranteed height per clickable row on a pointer (egui ships 18),
    // 32 pt — 48 dp — under a finger.
    s.interact_size = egui::vec2(40.0, if touch { TOUCH_ROW_PTS } else { ROW_PTS });
    s.window_margin = Margin::same(10);
    s.menu_margin = Margin::same(8);
    s.menu_spacing = 4.0;
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
/// exactly one look either way.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    let style = std::sync::Arc::new(style());
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

// ─── The mark ─────────────────────────────────────────────────────────────────

/// The app mark, **drawn rather than sampled**: the isometric map-tower cluster —
/// a city whose buildings are clad in map paper, seven towers on a gridded
/// plinth with the place pin planted on the tallest, orange roofs throughout.
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
/// **The table below is `assets/icon/cartopolis.svg` in its own 512-unit square**
/// (`UNIT` = half of it), not a second set of numbers in fractions of the radius:
/// the launcher icon is the source of truth for the shape, and a coordinate here
/// can be read straight off a path over there. `index.html`'s loader carries the
/// same table again as inline SVG — the web splash and this one are one screen
/// handed over mid-load (see `systems::splash`), and a mark that changes at the
/// handover is the tell that they are not. Change one, change all three.
///
/// Three deliberate simplifications against the launcher art, because this is
/// painted from egui primitives at a tenth of the size and every one of them
/// costs shapes on the very first frame: the tile's vertical gradient is its
/// middle tone (egui has no gradient primitive), each facade carries one motif
/// rather than the source's per-tower collage, and the teardrop pin is a disc
/// over a triangle. The silhouette, the palette and the placement — which is all
/// anyone reads at 44 px — are the source's exactly.
mod mark {
    use egui::Color32;

    /// Half the icon's square. Every coordinate below is in that square's units
    /// with `(UNIT, UNIT)` at the centre, so `paint_mark` maps them with one
    /// scale factor of `radius / UNIT`.
    pub const UNIT: f32 = 256.0;
    /// The launcher tile's corner radius.
    pub const PLATE_CORNER: f32 = 115.0;
    /// The isometric ratio: a unit across the ground is half a unit down the
    /// screen. Every roof, plinth edge and face shear in the table is this one
    /// number, which is what keeps the cluster on a single projection.
    pub const ISO: f32 = 0.5;

    /// Below this radius the facades, the plinth's grid and the body outlines
    /// are a smudge rather than detail — a 30 pt mark gives each tower face
    /// about 3 px of width — so they are dropped instead of drawn. Silhouette,
    /// roofs and pin carry the mark at that size.
    pub const DETAIL_RADIUS: f32 = 30.0;

    /// The plinth: centre of its top face, half its width, and the slab's depth.
    pub const PLINTH_C: [f32; 2] = [240.0, 335.0];
    pub const PLINTH_HALF: f32 = 195.0;
    pub const PLINTH_DEPTH: f32 = 16.0;
    /// Where the grid lines cross it, as fractions along each edge.
    pub const PLINTH_GRID: [f32; 3] = [0.25, 0.5, 0.75];

    /// The pin: centre of its head, the head's radius, its tip, and the dot.
    pub const PIN_C: [f32; 2] = [190.0, 73.0];
    pub const PIN_R: f32 = 27.3;
    pub const PIN_TIP_Y: f32 = 112.0;
    pub const PIN_DOT_R: f32 = 8.45;
    /// Width of the cream keyline around the pin. The pin is the only thing that
    /// overlaps the towers, and without it the orange sits straight on the
    /// paper-cream facade behind.
    pub const PIN_KEYLINE: f32 = 5.0;

    /// The ground shadow.
    pub const SHADOW_C: [f32; 2] = [240.0, 430.0];
    pub const SHADOW_R: [f32; 2] = [205.0, 42.0];

    /// What one tower's two visible faces are clad in. Every tower in the source
    /// wears a different fragment of map; at this size they reduce to three
    /// readable kinds.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Facade {
        /// City blocks: a park, streets across, one amber route down.
        Blocks,
        /// Coastline: sea below a shore, land above.
        Coast,
        /// Polder: strips of field with a canal down them.
        Polder,
    }

    /// One tower, on the front edge where its two faces meet.
    pub struct Tower {
        /// That edge: its `x`, and the `y` of its top (the roof's front vertex).
        pub x: f32,
        pub y: f32,
        /// How far the left and right faces reach from it.
        pub wl: f32,
        pub wr: f32,
        /// Body height, down the screen from `y`.
        pub h: f32,
        pub facade: Facade,
    }

    /// Back to front — the paint order *is* the depth order, so this array is
    /// not sortable by anything else.
    pub const TOWERS: [Tower; 7] = [
        Tower {
            x: 256.0,
            y: 159.0,
            wl: 26.0,
            wr: 26.0,
            h: 118.0,
            facade: Facade::Blocks,
        },
        Tower {
            x: 190.0,
            y: 142.0,
            wl: 30.0,
            wr: 30.0,
            h: 170.0,
            facade: Facade::Blocks,
        },
        Tower {
            x: 322.0,
            y: 225.0,
            wl: 24.0,
            wr: 24.0,
            h: 84.0,
            facade: Facade::Coast,
        },
        Tower {
            x: 124.0,
            y: 274.0,
            wl: 24.0,
            wr: 24.0,
            h: 68.0,
            facade: Facade::Polder,
        },
        Tower {
            x: 256.0,
            y: 199.0,
            wl: 30.0,
            wr: 30.0,
            h: 146.0,
            facade: Facade::Coast,
        },
        Tower {
            x: 190.0,
            y: 326.0,
            wl: 22.0,
            wr: 22.0,
            h: 48.0,
            facade: Facade::Polder,
        },
        Tower {
            x: 314.5,
            y: 286.0,
            wl: 26.0,
            wr: 56.0,
            h: 90.0,
            facade: Facade::Blocks,
        },
    ];

    // The map palette, off the icon source. These are not the UI palette: the
    // towers are printed maps, so they carry paper, ink, park green and water
    // blue rather than the chrome's graphite. The brand colours meet them at the
    // roofs and the pin, which is [`super::BRAND`] in both files. Note the plate
    // is still the *old* chrome slate — the mark is the logo, it did not go
    // monochrome with the UI, and it is a picture rather than a surface.
    /// The tile, as one tone: the source's `#2e424e` → `#1B2830` gradient in the
    /// middle, which is what an unlit slate reads as at any size.
    pub const PLATE: Color32 = Color32::from_rgb(0x25, 0x35, 0x3F);
    /// The ground shadow's ink, at the source's 40 % — carried in the constant
    /// rather than applied at the call site, so every colour in this table is a
    /// finished colour and `paint_mark`'s fade is the only thing that touches
    /// alpha.
    pub const SHADOW: Color32 = Color32::from_rgba_unmultiplied_const(0x0D, 0x15, 0x1A, 102);
    /// Every outline in the cluster: roof edges and the tower bodies.
    pub const EDGE: Color32 = Color32::from_rgba_unmultiplied_const(0x10, 0x18, 0x1E, 102);
    /// The plinth: top face, its grid, and the two side faces.
    pub const PLINTH_TOP: Color32 = Color32::from_rgb(0xE6, 0xDF, 0xCF);
    pub const PLINTH_GRID_INK: Color32 = Color32::from_rgb(0xD8, 0xD1, 0xBF);
    pub const PLINTH_LEFT: Color32 = Color32::from_rgb(0xC3, 0xBC, 0xAB);
    pub const PLINTH_RIGHT: Color32 = Color32::from_rgb(0xB1, 0xA9, 0x96);
    /// Map paper, and the streets printed on it.
    pub const PAPER: Color32 = Color32::from_rgb(0xF2, 0xEF, 0xEA);
    pub const STREET: Color32 = Color32::from_rgb(0xB4, 0xAC, 0x99);
    pub const PARK: Color32 = Color32::from_rgb(0x9D, 0xB0, 0x8D);
    pub const WATER: Color32 = Color32::from_rgb(0x5B, 0x81, 0x99);
    pub const WATER_EDGE: Color32 = Color32::from_rgb(0x46, 0x68, 0x7E);
    /// The lit line just off a shore.
    pub const WATER_LIGHT: Color32 = Color32::from_rgb(0x8F, 0xB0, 0xC0);
    /// A route highlighted on the map — the amber the UI reserves for a held
    /// state, muted to sit on paper.
    pub const ROUTE: Color32 = Color32::from_rgb(0xDF, 0xA4, 0x4E);
    /// How far the two faces are shaded apart. The right face takes the source's
    /// 16 % ink wash and the left its 5 % lift, which is the whole of what makes
    /// a cream box read as a solid rather than a hexagon.
    pub const FACE_SHADE: f32 = 0.16;
    pub const FACE_LIFT: f32 = 0.05;
}

/// `over` mixed into `base` by `k`, straight down the channels.
///
/// The mark's two face tones and nothing else: a tower's right face is the
/// source's 16 % ink wash over map paper and its left face a 5 % lift, which is
/// what makes a cream box read as a solid rather than as a hexagon.
fn mix(base: Color32, over: Color32, k: f32) -> Color32 {
    let ch = |b: u8, o: u8| (b as f32 + (o as f32 - b as f32) * k).round() as u8;
    Color32::from_rgb(
        ch(base.r(), over.r()),
        ch(base.g(), over.g()),
        ch(base.b(), over.b()),
    )
}

/// One tower face, in the sheared frame it is drawn in: `u` runs across the
/// face from its far edge, `v` down it, and the face's own slope carries the
/// isometric projection so a motif can be written as if it were on flat paper.
///
/// The left face slopes *down* to the right (its far edge is the leftmost point
/// of the tower and its near edge is the front edge); the right face slopes up.
/// That sign is the only difference between the two, which is why one struct
/// draws both.
#[derive(Clone, Copy)]
struct MarkFace {
    ox: f32,
    oy: f32,
    slope: f32,
    w: f32,
    h: f32,
    /// The right face, which takes the shade rather than the lift.
    dark: bool,
}

impl MarkFace {
    fn left(t: &mark::Tower) -> Self {
        Self {
            ox: t.x - t.wl,
            oy: t.y - t.wl * mark::ISO,
            slope: mark::ISO,
            w: t.wl,
            h: t.h,
            dark: false,
        }
    }
    fn right(t: &mark::Tower) -> Self {
        Self {
            ox: t.x,
            oy: t.y,
            slope: -mark::ISO,
            w: t.wr,
            h: t.h,
            dark: true,
        }
    }
    /// Face-local (`u` across, `v` down) → the icon's 512-unit square.
    fn at(&self, u: f32, v: f32) -> [f32; 2] {
        [self.ox + u, self.oy + self.slope * u + v]
    }
    /// The same, in fractions of the face — how every motif below is written, so
    /// a tower's cladding does not have to know how wide or tall it is.
    fn frac(&self, u: f32, v: f32) -> [f32; 2] {
        self.at(u * self.w, v * self.h)
    }
    /// The face's own outline, in the order it is drawn.
    fn corners(&self) -> [[f32; 2]; 4] {
        [
            self.frac(0.0, 0.0),
            self.frac(1.0, 0.0),
            self.frac(1.0, 1.0),
            self.frac(0.0, 1.0),
        ]
    }
}

/// Draw [the mark](mark) centred on `center`, out to `radius` (half the tile, so
/// the splash's 84 gives a 168 pt square), at `alpha` — 1.0 opaque; the splash
/// fades the whole screen out through it.
pub fn paint_mark(p: &egui::Painter, center: egui::Pos2, radius: f32, alpha: f32) {
    // `gamma_multiply` rather than a rebuilt `from_rgba_unmultiplied`: `Color32`
    // is premultiplied, so re-wrapping its channels as straight ones darkens
    // anything in the table that already carries an alpha (the shadow, every
    // outline).
    let a = |c: Color32| c.gamma_multiply(alpha);
    let s = radius / mark::UNIT;
    // Icon units → screen. Everything below is written in the source SVG's
    // coordinates and passes through here, which is what lets the two be
    // compared line by line.
    let q = |v: [f32; 2]| {
        egui::pos2(
            center.x + (v[0] - mark::UNIT) * s,
            center.y + (v[1] - mark::UNIT) * s,
        )
    };
    // A stroke thinner than a pixel is a stroke nobody can see: on the sign-in
    // card's 22 pt mark a 2-unit street is a third of a pixel, and this rule is
    // what keeps the roofs' outlines from disappearing entirely.
    let w = |units: f32| (units * s).max(0.8);
    let fill = |pts: Vec<egui::Pos2>, c: Color32| {
        p.add(egui::Shape::convex_polygon(pts, a(c), Stroke::NONE));
    };
    let detail = radius >= mark::DETAIL_RADIUS;

    // ── The tile ─────────────────────────────────────────────────────────────
    p.rect_filled(
        egui::Rect::from_center_size(center, egui::Vec2::splat(radius * 2.0)),
        CornerRadius::same((mark::PLATE_CORNER * s).round().clamp(0.0, 255.0) as u8),
        a(mark::PLATE),
    );
    p.add(egui::Shape::ellipse_filled(
        q(mark::SHADOW_C),
        egui::vec2(mark::SHADOW_R[0] * s, mark::SHADOW_R[1] * s),
        a(mark::SHADOW),
    ));

    // ── The plinth ───────────────────────────────────────────────────────────
    // A diamond seen at [`mark::ISO`], with a slab of depth under its two front
    // edges. Its four corners are named because the grid below is written as
    // fractions along them.
    let (cx, cy) = (mark::PLINTH_C[0], mark::PLINTH_C[1]);
    let (half, rise) = (mark::PLINTH_HALF, mark::PLINTH_HALF * mark::ISO);
    let (left, back) = ([cx - half, cy], [cx, cy - rise]);
    let (right, front) = ([cx + half, cy], [cx, cy + rise]);
    let down = |v: [f32; 2]| [v[0], v[1] + mark::PLINTH_DEPTH];
    fill(
        vec![q(left), q(front), q(down(front)), q(down(left))],
        mark::PLINTH_LEFT,
    );
    fill(
        vec![q(front), q(right), q(down(right)), q(down(front))],
        mark::PLINTH_RIGHT,
    );
    fill(vec![q(left), q(back), q(right), q(front)], mark::PLINTH_TOP);
    if detail {
        let lerp = |from: [f32; 2], to: [f32; 2], k: f32| {
            [
                from[0] + (to[0] - from[0]) * k,
                from[1] + (to[1] - from[1]) * k,
            ]
        };
        let grid = Stroke::new(w(1.8), a(mark::PLINTH_GRID_INK));
        for k in mark::PLINTH_GRID {
            // One family parallel to each pair of edges — a street grid on the
            // ground, in the same projection the towers stand in.
            p.line_segment([q(lerp(left, back, k)), q(lerp(front, right, k))], grid);
            p.line_segment([q(lerp(left, front, k)), q(lerp(back, right, k))], grid);
        }
    }

    // ── The towers ───────────────────────────────────────────────────────────
    let shade = |c: Color32| mix(c, INK, mark::FACE_SHADE);
    let lift = |c: Color32| mix(c, Color32::WHITE, mark::FACE_LIFT);
    for t in &mark::TOWERS {
        let faces = [MarkFace::left(t), MarkFace::right(t)];
        for f in faces {
            let tone = |c: Color32| {
                if f.dark {
                    shade(c)
                } else {
                    lift(c)
                }
            };
            fill(f.corners().map(&q).to_vec(), tone(mark::PAPER));
            if detail {
                paint_facade(p, &q, &w, &f, t.facade, &|c| a(tone(c)));
            }
        }
        // The roof is the same diamond as the plinth, one tower wide, and it is
        // the brand orange in both files — [`BRAND`], not the chrome accent.
        let roof = vec![
            q([t.x, t.y]),
            q([t.x + t.wr, t.y - t.wr * mark::ISO]),
            q([t.x + t.wr - t.wl, t.y - (t.wr + t.wl) * mark::ISO]),
            q([t.x - t.wl, t.y - t.wl * mark::ISO]),
        ];
        let edge = Stroke::new(w(1.4), a(mark::EDGE));
        p.add(egui::Shape::convex_polygon(
            roof,
            a(BRAND),
            if detail { edge } else { Stroke::NONE },
        ));
        if detail {
            // The body outlines, over the roof rather than under it — without
            // them a tower standing in front of another dissolves into it, same
            // paper and same tone.
            for f in faces {
                p.add(egui::Shape::closed_line(f.corners().map(&q).to_vec(), edge));
            }
        }
    }

    // ── The pin ──────────────────────────────────────────────────────────────
    // A disc over a triangle rather than the source's teardrop path: at this
    // size the two are the same picture, and egui has no path primitive that
    // would carry the curve without tessellating it here. Drawn twice — cream
    // keyline first, orange body inside it — because the pin is the one element
    // that overlaps a facade, and the outline is what lifts it off the paper.
    let pin = q(mark::PIN_C);
    let tip = q([mark::PIN_C[0], mark::PIN_TIP_Y]);
    let pin_r = mark::PIN_R * s;
    let key = mark::PIN_KEYLINE * s;
    let skirt = |r: f32, tip: egui::Pos2| {
        vec![
            egui::pos2(pin.x - r * 0.87, pin.y + r * 0.49),
            egui::pos2(pin.x + r * 0.87, pin.y + r * 0.49),
            tip,
        ]
    };
    fill(skirt(pin_r + key, egui::pos2(tip.x, tip.y + key)), CREAM);
    p.circle_filled(pin, pin_r + key, a(CREAM));
    fill(skirt(pin_r, tip), BRAND);
    p.circle_filled(pin, pin_r, a(BRAND));
    p.circle_filled(pin, mark::PIN_DOT_R * s, a(CREAM));
}

/// One facade's motif, written in fractions of the face so the seven towers
/// share three drawings.
///
/// Kept out of [`paint_mark`] because it is the part that is *decoration*: the
/// mark reads without any of it (see [`mark::DETAIL_RADIUS`]), and separating
/// the two keeps that obvious.
fn paint_facade(
    p: &egui::Painter,
    q: &impl Fn([f32; 2]) -> egui::Pos2,
    w: &impl Fn(f32) -> f32,
    f: &MarkFace,
    facade: mark::Facade,
    a: &impl Fn(Color32) -> Color32,
) {
    let band = |v0: f32, v1: f32, c: Color32| {
        p.add(egui::Shape::convex_polygon(
            vec![
                q(f.frac(0.0, v0)),
                q(f.frac(1.0, v0)),
                q(f.frac(1.0, v1)),
                q(f.frac(0.0, v1)),
            ],
            a(c),
            Stroke::NONE,
        ));
    };
    let across = |v: f32, c: Color32, units: f32| {
        p.line_segment(
            [q(f.frac(0.0, v)), q(f.frac(1.0, v))],
            Stroke::new(w(units), a(c)),
        );
    };
    match facade {
        mark::Facade::Blocks => {
            band(0.10, 0.20, mark::PARK);
            for v in [0.34, 0.62, 0.88] {
                across(v, mark::STREET, 2.0);
            }
            // The route down the block — the one warm line on the paper.
            p.line_segment(
                [q(f.frac(0.68, 0.0)), q(f.frac(0.68, 1.0))],
                Stroke::new(w(3.0), a(mark::ROUTE)),
            );
        }
        mark::Facade::Coast => {
            band(0.58, 1.0, mark::WATER);
            across(0.58, mark::WATER_EDGE, 2.4);
            across(0.66, mark::WATER_LIGHT, 1.6);
            band(0.16, 0.26, mark::PARK);
            across(0.10, mark::ROUTE, 2.8);
        }
        mark::Facade::Polder => {
            // Strips of field, and the canal that drains them.
            for (i, v) in [0.06_f32, 0.28, 0.50, 0.72].into_iter().enumerate() {
                band(
                    v,
                    v + 0.16,
                    if i % 2 == 0 { mark::PARK } else { mark::PAPER },
                );
                across(v, mark::WATER, 1.4);
            }
            p.line_segment(
                [q(f.frac(0.45, 0.0)), q(f.frac(0.45, 1.0))],
                Stroke::new(w(2.4), a(mark::WATER)),
            );
        }
    }
}

// ─── Wordmark ─────────────────────────────────────────────────────────────────

/// The wordmark, in two tones: **cream `carto`, accent `polis`**, lower case.
///
/// Not a decoration — it is the account GUI's wordmark
/// (`jeroen/cartopy`'s `.app-header .wordmark`, `carto<b>polis</b>`), and that
/// app talks to this simulator's admin surface. Two products that share a palette
/// and a backend should not write their own name two different ways, so the split
/// and the colours are copied from there rather than re-invented, and
/// `index.html`'s loader carries the same two spans.
pub const WORDMARK_HEAD: &str = "carto";
pub const WORDMARK_TAIL: &str = "polis";

/// The tail's orange — [`BRAND`], and no longer the accent.
///
/// **Named rather than written at each of the three sites, because they had
/// already drifted.** This was [`ACCENT_TEXT`] (`#FFA470`) here while
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
pub fn paint_wordmark(p: &egui::Painter, center: egui::Pos2, size: f32, alpha: f32) {
    let a = |c: Color32| {
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * alpha) as u8)
    };
    let font = FontId::new(size, brand_family());
    let head = p.layout_no_wrap(WORDMARK_HEAD.to_owned(), font.clone(), a(CREAM));
    let tail = p.layout_no_wrap(WORDMARK_TAIL.to_owned(), font, a(WORDMARK_TAIL_COLOR));
    let total = head.size().x + tail.size().x;
    let left = center.x - total * 0.5;
    let top = center.y - head.size().y * 0.5;
    let head_w = head.size().x;
    p.galley(egui::pos2(left, top), head, a(CREAM));
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
                .color(CREAM),
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
        .fill(OVERLAY)
        .stroke(Stroke::new(1.0, OVERLAY_LINE))
        .corner_radius(CornerRadius::same(RADIUS_LG))
        .inner_margin(Margin::symmetric(11, 8))
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
const PRIMARY_BUTTON_HEIGHT: f32 = 34.0;

/// **The** call to action on a screen: filled with the accent, labelled in
/// [`INK`] at [`SIZE_VALUE`], taller than an ordinary button, and at least
/// `min_width` points wide.
///
/// There should be at most one per screen, and it should be the thing you came to
/// do — Sign in, Start driving. Everything else is `ui.button`.
///
/// Exists because the app already had two primary buttons that did not look
/// alike: the sign-in CTA was a hand-built filled `Button` and the Route panel's
/// equally-primary "▶ Driver mode" was a plain `ui.button`, so the same rank read
/// as two different ones. `INK` on `ACCENT` and not white is the palette's rule for
/// text on the accent (12.7:1 against 1.4:1) and is unit-tested below.
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
/// **And it answers the pointer.** The fill was a flat [`ACCENT`] in every
/// state, so the one control on the screen whose whole job is to be pressed gave
/// nothing back when you pressed it — no hover, no press, no focus ring, on a
/// button that then waits on a network round trip. It lifts to
/// [`ACCENT_BRIGHT`] under the pointer, sinks to [`ACCENT_PRESSED`] while held,
/// and wears an [`ACCENT_RING`] outline when the keyboard is on it. The state
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
        ACCENT_MUTED
    } else if held {
        ACCENT_PRESSED
    } else if hovered {
        ACCENT_BRIGHT
    } else {
        ACCENT
    };
    ui.add(
        egui::Button::new((
            egui::Atom::grow(),
            egui::RichText::new(label)
                .size(SIZE_VALUE)
                .strong()
                .color(INK),
            egui::Atom::grow(),
        ))
        .fill(fill)
        .stroke(if focused {
            Stroke::new(FOCUS_RING, ACCENT_RING)
        } else {
            Stroke::NONE
        })
        .corner_radius(CornerRadius::same(RADIUS))
        .min_size(egui::vec2(min_width, height)),
    )
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
/// A single [`CREAM`] ring is 16.7:1 over the panel and invisible over white
/// paper; a single [`INK`] one is the reverse. The pair cannot both disappear, so
/// the outline is legible everywhere without the palette having to guess what is
/// under it. It used to be the light ring alone, and the monochrome chrome makes
/// that worse rather than better: there is no longer a saturated colour to fall
/// back on.
///
/// [`CREAM`] and not the accent for the inner ring: an accent ring is the
/// *pressed* state, and would say the wrong thing.
fn paint_focus_ring(ui: &egui::Ui, resp: &egui::Response, rect: egui::Rect, round: bool) {
    if !resp.has_focus() {
        return;
    }
    let p = ui.painter();
    let ring = rect.expand(FOCUS_RING);
    // Outer first, wider, so the light ring is drawn over the middle of it and
    // what is left either side is a dark edge on both flanks.
    for (width, colour) in [(FOCUS_RING * 2.0, INK), (FOCUS_RING, CREAM)] {
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
/// engaged plate — [`ACCENT`] with [`INK`] on it, the same inversion every "this
/// is on" control in the app uses.
pub fn round_button(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    selected: bool,
) -> egui::Response {
    let size = egui::Vec2::splat(ROUND_BUTTON_SIZE);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = resp.hovered();
    let (fill, ring, ink) = match (selected, hovered) {
        (true, _) => (ACCENT, ACCENT_RING, INK),
        (false, true) => (SURFACE_HOVER, ACCENT, CREAM),
        (false, false) => (OVERLAY, OVERLAY_LINE, TEXT),
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

/// The fill of a control that is a *guide* rather than a plate — the
/// thumbstick's well, which is [`ROUND_BUTTON_SIZE`]-and-a-half across and at
/// [`OVERLAY`]'s opacity read as a hole punched in the map rather than as
/// something to put a thumb on. Same graphite, half the coverage.
///
/// Nothing legible sits on it, which is what lets it go this far — [`OVERLAY`]'s
/// own note is a contrast argument about the text inside a box, and there is no
/// text here. The floor is the other direction: at alpha 90 the well vanished
/// into pale beige map, and a stick you cannot see the travel of is a pad.
pub const OVERLAY_SOFT: Color32 = Color32::from_rgba_unmultiplied_const(0x13, 0x19, 0x1B, 140);

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
/// wears the same light [`ACCENT`] plate a selected dock disc does, so "this is
/// on" is one treatment everywhere on the map. **It inverts rather than tinting**,
/// which is what the monochrome palette costs and buys: there is no accent hue
/// left to fill a plate with, and a dark-glyph-on-light-plate toggle is
/// unmistakable against a map at any brightness in a way a slightly different
/// grey would not be. `tone` is the rim that engagement carries: [`ACCENT_RING`]
/// normally, [`ACCENT_BRIGHT`] for the *held* states (north-up lock,
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
        (true, _) => (ACCENT, tone, INK),
        (false, true) => (SURFACE_HOVER, ACCENT, CREAM),
        (false, false) => (OVERLAY, OVERLAY_LINE, TEXT),
    };
    let radius = size * 0.5 - 1.0;
    let p = ui.painter();
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
/// disc is [`ACCENT`] now — a light plate — and the map's brightest regions are
/// the sky and the near-white paper the basemap is printed on, so over those the
/// plate and its lighter rim both dissolve and the control disappears at exactly
/// the moment it is *on*. That is the same failure the *off* state used to have
/// over bright tiles, which is why its ring went to 1.5 pt; the light plate needs
/// the other half of the answer.
///
/// Black at low alpha rather than [`INK`], for the reason [`OVERLAY_LINE`] is
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

fn paint_disc(p: &egui::Painter, center: egui::Pos2, radius: f32, d: Disc<'_>) {
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

/// Diameter of the header's icon medallion. Small enough to sit on one text
/// line, large enough that the dock's glyph survives the shrink. It is not a
/// target — nothing clicks it — so it does not grow on the touch tier.
const HEADER_MEDALLION: f32 = 24.0;

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
    38.0_f32.max(close_button_size() + 2.0 * HEADER_INSET)
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
    let height = row_height().max(ui.spacing().interact_size.y);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let hovered = enabled && resp.hovered();
    let ink = match (enabled, selected, hovered) {
        (false, _, _) => TEXT_DIM.gamma_multiply(0.6),
        (_, true, _) => ACCENT_TEXT,
        (_, false, true) => CREAM,
        _ => TEXT,
    };
    let p = ui.painter();
    if hovered || resp.is_pointer_button_down_on() {
        let fill = if resp.is_pointer_button_down_on() {
            ACCENT_MUTED
        } else {
            SURFACE_HOVER
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
        p.rect_filled(bar, CornerRadius::same(2), ACCENT);
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
        egui::FontId::proportional(SIZE_BODY),
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
/// selected segment is the *pale* one.
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
        (ACCENT, ACCENT_RING, INK)
    } else {
        (SURFACE_RAISED, LINE, TEXT)
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
    // Frameless at rest: a header carries one obvious action, and boxing it makes
    // the title bar look like it has a second panel in it.
    let (fill, ring, ink) = if resp.is_pointer_button_down_on() {
        (ACCENT_MUTED, ACCENT_RING, CREAM)
    } else if resp.hovered() {
        (SURFACE_HOVER, ACCENT, CREAM)
    } else {
        (Color32::TRANSPARENT, Color32::TRANSPARENT, TEXT_DIM)
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
    let (fill, ring, ink) = if resp.is_pointer_button_down_on() {
        (ACCENT_MUTED, ACCENT_RING, CREAM)
    } else if resp.hovered() {
        (SURFACE_HOVER, ACCENT, CREAM)
    } else {
        (SURFACE_RAISED, LINE, TEXT_DIM)
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
    let avail = ui.available_rect_before_wrap();
    let band = egui::Rect::from_min_size(avail.min, egui::vec2(avail.width(), header_height()));
    let radius = CornerRadius::same(RADIUS);

    let p = ui.painter();
    p.add(
        Shadow {
            offset: [0, 3],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(90),
        }
        .as_shape(band, radius),
    );
    p.rect(
        band,
        radius,
        SURFACE_RAISED,
        Stroke::new(1.0, LINE),
        egui::StrokeKind::Inside,
    );
    p.line_segment(
        [
            egui::pos2(band.left() + HEADER_INSET, band.top() + 1.5),
            egui::pos2(band.right() - HEADER_INSET, band.top() + 1.5),
        ],
        Stroke::new(1.0, SURFACE_HOVER),
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
                .max_rect(band.shrink2(egui::Vec2::splat(HEADER_INSET)))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        row.spacing_mut().item_spacing.x = 8.0;
        close = close_button(&mut row).clicked();
        extra(&mut row);
        row.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            let (medallion, _) =
                ui.allocate_exact_size(egui::Vec2::splat(HEADER_MEDALLION), egui::Sense::hover());
            paint_disc(
                ui.painter(),
                medallion.center(),
                HEADER_MEDALLION * 0.5 - 0.5,
                Disc {
                    // The dock disc that opened this panel is engaged, so the
                    // medallion has to be the engaged plate too — they are one
                    // object at two sizes, and a header wearing the *old* state
                    // of its own button is the drift this function exists to stop.
                    fill: ACCENT,
                    ring: ACCENT_RING,
                    ink: INK,
                    icon,
                    icon_size: 13.0,
                    ring_width: 1.0,
                },
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(title)
                        .font(FontId::new(SIZE_TITLE, brand_family()))
                        .color(CREAM),
                )
                .truncate()
                .selectable(false),
            );
        });
    }
    ui.advance_cursor_after_rect(band);
    // Clears the shadow, which reaches below the band.
    ui.add_space(4.0);
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
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(title)
            .size(SIZE_SUB)
            .strong()
            .color(CREAM),
    );
}

/// A line under a control stating a constraint or a stored intention — *not* a
/// failure. Plain dim text rather than a warning colour: it is a fact about the
/// build or the world, not something the user did wrong.
pub fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(SIZE_CHIP)
            .color(TEXT_DIM)
            .italics(),
    );
}

/// The body of a list with nothing in it.
///
/// `hint` is **not optional**, and that is the whole design: "No places yet" on
/// its own is a dead end, and the Places panel's existing empty state — which
/// names the two buttons and says what each does — is the model this generalises.
/// Four lists in the client currently draw literally nothing when empty (an empty
/// chat channel, a fresh AI conversation, an empty folder, the texture library),
/// which is indistinguishable from a panel that failed to load.
pub fn empty_state(ui: &mut egui::Ui, headline: &str, hint: &str) {
    ui.add_space(12.0);
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(headline)
                .size(SIZE_BODY)
                .color(TEXT_DIM),
        );
        ui.add_space(4.0);
        ui.add(egui::Label::new(egui::RichText::new(hint).size(SIZE_SUB).color(TEXT_DIM)).wrap());
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
        ui.add(egui::Spinner::new().size(SIZE_BODY).color(ACCENT));
        ui.label(egui::RichText::new(label).size(SIZE_SUB).color(TEXT_DIM));
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
/// On a faint [`ERROR`]-tinted chip rather than as bare coloured text, so that an
/// error can never again be styled as a footnote — the Search panel reported a
/// geocoding failure through `ui.weak()`, which is the *quietest* tone in the
/// palette, for the one string that most needs to be read.
#[must_use]
pub fn inline_error(ui: &mut egui::Ui, message: &str) -> bool {
    let mut dismissed = false;
    egui::Frame::NONE
        .fill(ERROR.gamma_multiply(0.16))
        .stroke(Stroke::new(1.0, ERROR.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(RADIUS_SM))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(message).size(SIZE_SUB).color(ERROR))
                        .wrap(),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                dismissed = action_button(ui, "Dismiss").clicked();
            });
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
            egui::Button::new(egui::RichText::new(confirm_label).color(INK).strong())
                .fill(ERROR)
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
    ui.scope(|ui| {
        let square = CornerRadius::same(RADIUS_SM);
        let w = &mut ui.style_mut().visuals.widgets;
        w.noninteractive.corner_radius = square;
        w.inactive.corner_radius = square;
        w.hovered.corner_radius = square;
        w.active.corner_radius = square;
        ui.checkbox(checked, text)
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
/// system runs before it, by construction (`lib.rs`, phase 7d).
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
        let ink = if resp.is_pointer_button_down_on() || shown {
            ACCENT
        } else {
            TEXT_DIM
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

    /// The palette's whole claim is that it is *legible*, and a colour is very
    /// easy to nudge into looking nicer and reading worse. Pin the pairs that
    /// actually occur: text on a panel, text on a selected row, text on the
    /// pressed-button orange.
    #[test]
    fn text_colours_clear_their_backgrounds() {
        for (name, fg, bg, min) in [
            ("body on panel", TEXT, SURFACE, 4.5),
            ("body on raised", TEXT, SURFACE_RAISED, 4.5),
            ("dim on panel", TEXT_DIM, SURFACE, 4.5),
            ("heading on panel", CREAM, SURFACE, 4.5),
            ("link on panel", ACCENT_TEXT, SURFACE, 4.5),
            ("selected row", CREAM, ACCENT_MUTED, 4.5),
            ("label on pressed button", CREAM, ACCENT_MUTED, 4.5),
            ("ink on an engaged plate", INK, ACCENT, 4.5),
            // The three fills [`primary_button_sized`] moves between. The label
            // is [`INK`] in all of them — which since the repaint is the *easy*
            // direction, all three being light tones, so these now guard against
            // a future accent drifting dark rather than against the press going
            // too far down.
            ("primary label, hovered", INK, ACCENT_BRIGHT, 4.5),
            ("primary label, pressed", INK, ACCENT_PRESSED, 4.5),
            ("warning on panel", WARN, SURFACE, 4.5),
            ("error on panel", ERROR, SURFACE, 4.5),
            // Not text: a fill, a rule or a focus ring, which WCAG puts at 3:1.
            ("focus ring on panel", ACCENT, SURFACE, 3.0),
        ] {
            let got = contrast(fg, bg);
            assert!(got >= min, "{name}: contrast {got:.2} < {min}");
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

    /// **Every surface a colour can land on, not just the panel fill.**
    ///
    /// [`text_colours_clear_their_backgrounds`] pins eleven pairs and almost
    /// every one is against [`SURFACE`] or an accent fill. That is the easy half
    /// of the palette: the ramp exists precisely so things *can* sit on
    /// [`SURFACE_RAISED`] and [`SURFACE_HOVER`], and egui puts them there without
    /// being asked — `weak_text_color` inside an `open` menu is [`TEXT_DIM`] on
    /// [`SURFACE_HOVER`], which measured 4.40 and is the failure this test was
    /// written for.
    ///
    /// [`OVERLAY`] is the other half, and it was worse, because its background is
    /// not in this file at all.
    #[test]
    fn text_clears_every_surface_it_can_land_on() {
        let over_bright = over(OVERLAY, BRIGHT_MAP);
        let over_dark = over(OVERLAY, DARK_MAP);
        let glass_bright = over(GLASS, BRIGHT_MAP);
        let glass_dark = over(GLASS, DARK_MAP);
        let chip_bright = over(LABEL_CHIP, BRIGHT_MAP);
        let halo_bright = over(LABEL_HALO, BRIGHT_MAP);
        let halo_dark = over(LABEL_HALO, DARK_MAP);

        for (name, fg, bg, min) in [
            // The raised step: buttons at rest, striped rows, the header band.
            ("dim on raised", TEXT_DIM, SURFACE_RAISED, 4.5),
            ("heading on raised", CREAM, SURFACE_RAISED, 4.5),
            ("link on raised", ACCENT_TEXT, SURFACE_RAISED, 4.5),
            ("warning on raised", WARN, SURFACE_RAISED, 4.5),
            ("error on raised", ERROR, SURFACE_RAISED, 4.5),
            (
                "held-state rim on raised",
                ACCENT_BRIGHT,
                SURFACE_RAISED,
                4.5,
            ),
            // The glyph on an engaged plate, which is the one place in the app
            // where dark type sits on a light fill.
            ("ink on an engaged plate", INK, ACCENT, 4.5),
            ("ink on an engaged plate's rim", INK, ACCENT_RING, 4.5),
            // The hover step — and an *open* menu button sits here at rest, so
            // this is not a transient a mouse merely passes through.
            ("body on hover", TEXT, SURFACE_HOVER, 4.5),
            ("dim on hover", TEXT_DIM, SURFACE_HOVER, 4.5),
            ("heading on hover", CREAM, SURFACE_HOVER, 4.5),
            ("link on hover", ACCENT_TEXT, SURFACE_HOVER, 4.5),
            ("error on hover", ERROR, SURFACE_HOVER, 4.5),
            // Text-edit wells and code blocks.
            ("body on ink", TEXT, INK, 4.5),
            ("dim on ink", TEXT_DIM, INK, 4.5),
            ("link on ink", ACCENT_TEXT, INK, 4.5),
            // The boxes over the map, against the map — both ends of it.
            ("body on an overlay over bright map", TEXT, over_bright, 4.5),
            (
                "dim on an overlay over bright map",
                TEXT_DIM,
                over_bright,
                4.5,
            ),
            (
                "heading on an overlay over bright map",
                CREAM,
                over_bright,
                4.5,
            ),
            (
                "link on an overlay over bright map",
                ACCENT_TEXT,
                over_bright,
                4.5,
            ),
            (
                "warning on an overlay over bright map",
                WARN,
                over_bright,
                4.5,
            ),
            (
                "error on an overlay over bright map",
                ERROR,
                over_bright,
                4.5,
            ),
            ("body on an overlay over dark map", TEXT, over_dark, 4.5),
            ("dim on an overlay over dark map", TEXT_DIM, over_dark, 4.5),
            // The chip has to hold over *anything*: a few words on an arbitrary
            // tile with no panel anywhere near them.
            ("chip label over bright map", CREAM, chip_bright, 4.5),
            ("chip dim over bright map", TEXT_DIM, chip_bright, 4.5),
            // And the halo, the same promise inverted: deep teal ink on its
            // pale-teal halo, over tarmac at one end and open water at the
            // other.
            ("street name over bright map", LABEL_INK, halo_bright, 4.5),
            ("street name over dark map", LABEL_INK, halo_dark, 4.5),
            // **The panels themselves, since they became glass.** A docked panel
            // used to be opaque, so `SURFACE` above covered it; it is [`GLASS`]
            // now and the map is behind every label in it. This is the row that
            // holds the alpha honest — a panel made prettier by being more
            // transparent fails here, in the case that matters (a form of dim
            // hint text over white paper) rather than in review.
            ("body on a panel over bright map", TEXT, glass_bright, 4.5),
            (
                "dim on a panel over bright map",
                TEXT_DIM,
                glass_bright,
                4.5,
            ),
            (
                "heading on a panel over bright map",
                CREAM,
                glass_bright,
                4.5,
            ),
            (
                "link on a panel over bright map",
                ACCENT_TEXT,
                glass_bright,
                4.5,
            ),
            (
                "warning on a panel over bright map",
                WARN,
                glass_bright,
                4.5,
            ),
            ("error on a panel over bright map", ERROR, glass_bright, 4.5),
            ("body on a panel over dark map", TEXT, glass_dark, 4.5),
            ("dim on a panel over dark map", TEXT_DIM, glass_dark, 4.5),
        ] {
            let got = contrast(fg, bg);
            assert!(got >= min, "{name}: contrast {got:.2} < {min}");
        }
    }

    /// **The state ladder rises, and in a monochrome palette that is the whole
    /// design.** Rest → hover → pressed → engaged, each one lighter than the last:
    /// with no hue left to say "this control is doing something", lightness is the
    /// only channel there is, and a ladder that doubles back is a UI where two
    /// different states look the same.
    ///
    /// Worth a test rather than a comment because each rung is picked for its own
    /// reasons — the pressed fill has to carry [`CREAM`], the engaged plate has to
    /// carry [`INK`], the hover step has to be visible over a map — and nothing
    /// but this asserts they still come in order afterwards.
    #[test]
    fn the_state_ladder_rises() {
        let rungs = [
            ("rest", SURFACE_RAISED),
            ("hover", SURFACE_HOVER),
            ("pressed", ACCENT_MUTED),
            ("engaged", ACCENT),
        ];
        for pair in rungs.windows(2) {
            let ((lo_name, lo), (hi_name, hi)) = (pair[0], pair[1]);
            assert!(
                luminance(hi) > luminance(lo),
                "{hi_name} ({:.3}) is not lighter than {lo_name} ({:.3})",
                luminance(hi),
                luminance(lo)
            );
        }
        // And the two ends are far enough apart to be told apart at a glance,
        // which "strictly increasing" alone does not promise.
        let got = contrast(ACCENT, SURFACE_RAISED);
        assert!(got >= 3.0, "engaged against rest: contrast {got:.2} < 3.0");
    }

    /// **A control has to be findable before its label matters**, and WCAG puts
    /// that boundary at 3:1 (1.4.11) — the same bar as a focus ring, for the same
    /// reason: it is the only thing saying *here is a thing you can press*.
    ///
    /// This is the check the palette never had and was furthest from passing. A
    /// button at rest is [`SURFACE_RAISED`] on [`SURFACE`], which is 1.24:1 of
    /// fill difference, framed by a [`LINE`] hairline that used to measure
    /// 1.92:1. Neither channel identified the control; what had been carrying it
    /// is that panels are dense enough for position to do the work.
    ///
    /// Two pairs are deliberately *not* asserted, and saying so is the point of
    /// writing them down: `SURFACE_HOVER` against `SURFACE_RAISED` (1.27) is a
    /// *change* of state rather than the identification of a control, and the
    /// accent ring arriving with it is what carries hover; and `LINE` against
    /// `SURFACE_HOVER` only ever appears on an `open` menu button, where the
    /// popup below it is the thing being looked at.
    #[test]
    fn a_control_is_distinguishable_from_the_surface_it_sits_on() {
        let v = visuals();
        for (name, got) in [
            (
                "a button's border on a panel",
                contrast(v.widgets.inactive.bg_stroke.color, SURFACE),
            ),
            (
                "the hover ring against the fill it encloses",
                contrast(v.widgets.hovered.bg_stroke.color, SURFACE_HOVER),
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
                contrast(v.widgets.active.bg_stroke.color, SURFACE),
            ),
            (
                "the engaged plate against the panel it sits on",
                contrast(ACCENT, SURFACE),
            ),
            ("the glyph on an engaged plate", contrast(INK, ACCENT)),
        ] {
            assert!(got >= 3.0, "{name}: contrast {got:.2} < 3.0");
        }

        // **The focus ring is two rings and has to be checked as two.** It is
        // drawn *outside* the control, so its background is not in the palette at
        // all — it is the map, at whatever brightness the imagery and the hour
        // produce, or a panel, or the light plate of the control it encircles.
        // `paint_focus_ring` answers that with a dark ring under a light one, and
        // the property to assert is not that either clears 3:1 everywhere, but
        // that **at least one of them does, on every background it can land on**.
        for (name, bg) in [
            ("a panel", SURFACE),
            ("an overlay disc over bright map", over(OVERLAY, BRIGHT_MAP)),
            ("an overlay disc over dark map", over(OVERLAY, DARK_MAP)),
            ("the light plate of an engaged disc", ACCENT),
            ("bare bright basemap", BRIGHT_MAP),
            ("bare dark basemap", DARK_MAP),
        ] {
            let best = contrast(CREAM, bg).max(contrast(INK, bg));
            assert!(
                best >= 3.0,
                "the keyboard-focus ring on {name}: neither half clears 3:1 \
                 (best {best:.2})"
            );
        }
    }

    /// The palette's own prose is a *claim*, and prose does not recompute.
    ///
    /// Three numbers in this file were wrong when this was written, all about
    /// the same pair and none checkable by reading: [`INK`] on the old accent was
    /// written as 5.9 in the module note and 6.0 further down, and the
    /// alternative it was compared against — "only 3.1:1" — was
    /// [`Color32::WHITE`]'s ratio, not [`CREAM`]'s. Pin the ratios the file
    /// commits to in writing, so editing a colour and not the paragraph beside it
    /// is a failing test rather than a stale comment.
    ///
    /// It earned its keep at the repaint: every one of these numbers moved when
    /// the chrome went monochrome, and the paragraphs quoting them are four
    /// screens away from the constants.
    #[test]
    fn the_documented_ratios_are_the_measured_ones() {
        for (claim, fg, bg, stated) in [
            // 11.8461 — the module note said "11.9:1" from the day it was
            // written, which is this rounded the wrong way. Nobody could have
            // caught that by reading it; that is the point of the test.
            ("TEXT on SURFACE", TEXT, SURFACE, 13.5),
            ("TEXT_DIM on SURFACE", TEXT_DIM, SURFACE, 8.2),
            ("ACCENT_TEXT on SURFACE", ACCENT_TEXT, SURFACE, 17.8),
            ("ACCENT on SURFACE", ACCENT, SURFACE, 11.6),
            ("INK on ACCENT", INK, ACCENT, 12.7),
            ("CREAM on ACCENT_MUTED", CREAM, ACCENT_MUTED, 8.0),
            ("LINE on SURFACE", LINE, SURFACE, 3.7),
            (
                "ACCENT_RING on ACCENT_MUTED",
                ACCENT_RING,
                ACCENT_MUTED,
                7.2,
            ),
            ("CREAM on ACCENT (the rejected pairing)", CREAM, ACCENT, 1.4),
        ] {
            // Compare at the precision the prose actually claims: the docs say
            // "11.9:1", so the test that keeps them honest is whether the
            // measurement *rounds* to that, not whether it equals it. An
            // absolute tolerance here would be a second, unstated claim.
            let got = contrast(fg, bg);
            assert!(
                (got * 10.0).round() / 10.0 == stated,
                "{claim}: the docs say {stated:.1}:1, the colours measure {got:.2}:1"
            );
        }
        // …and the pairing the whole `active` fill argument rests on. It used to
        // fail because the accent was too *dark* for cream (2.67); it now fails
        // far harder because the accent is too light (1.45). Either way an
        // accent-filled button cannot carry a cream label, which is why both of
        // `active`'s fills are ACCENT_MUTED and why anything actually filled with
        // ACCENT is labelled in INK.
        let cream_on_accent = contrast(CREAM, ACCENT);
        assert!(
            cream_on_accent < 3.0,
            "CREAM on ACCENT measures {cream_on_accent:.2} — if that is now over 3.0 the \
             note by `active` about why buttons take ACCENT_MUTED needs rewriting"
        );
        // The other half of that rule: what an ACCENT plate *is* labelled in.
        let ink_on_accent = contrast(INK, ACCENT);
        assert!(
            ink_on_accent >= 4.5,
            "INK on ACCENT measures {ink_on_accent:.2} — the engaged plate carries \
             a glyph, and this is the pair that has to hold"
        );
    }

    /// egui derives three app-wide text colours from the widget table rather
    /// than exposing them directly — plain text from `noninteractive`, weak text
    /// from `weak_text_color`, and **`RichText::strong()` from `widgets.active`**.
    /// That last one is the trap: `active` is also the pressed-button state, so
    /// picking its `fg_stroke` to suit the fill silently recolours every bold
    /// value in the app. Assert the three land where the palette says.
    #[test]
    fn derived_text_colours_come_from_the_palette() {
        let v = visuals();
        assert_eq!(v.text_color(), TEXT);
        assert_eq!(v.weak_text_color(), TEXT_DIM);
        assert_eq!(v.strong_text_color(), CREAM);
    }

    /// The wordmark is the app's name split in two, and nothing else. It is
    /// written in three places — here, `index.html`'s loader and the account
    /// GUI's header — so the halves are exactly where a typo would ship as a
    /// misspelt product name that still looks deliberate.
    #[test]
    fn the_wordmark_halves_spell_the_app() {
        assert_eq!(format!("{WORDMARK_HEAD}{WORDMARK_TAIL}"), "cartopolis");
    }

    /// The pin is planted on the tallest tower, and "planted" is exact: its tip
    /// lands on that tower's roof apex rather than near it.
    ///
    /// Worth a test because the two are separate rows of the table — a tower
    /// grown a few units taller, or the cluster restacked, leaves the pin
    /// floating over a gap or buried in a roof, and at 44 px that reads as a
    /// blurry mark rather than as a mistake.
    #[test]
    fn the_pin_is_planted_on_the_tallest_tower() {
        let tallest = mark::TOWERS
            .iter()
            .max_by(|a, b| a.h.total_cmp(&b.h))
            .expect("the cluster has towers");
        assert_eq!(
            mark::PIN_C[0],
            tallest.x,
            "the pin is not over the tallest tower's front edge"
        );
        // The roof's far vertex, which is where the front edge's top plus both
        // face widths lands in this projection.
        let apex = tallest.y - (tallest.wl + tallest.wr) * mark::ISO;
        assert!(
            (mark::PIN_TIP_Y - apex).abs() < 1.0,
            "the pin's tip is at {}, the roof it stands on at {apex}",
            mark::PIN_TIP_Y
        );
    }

    /// Every tower stands on the plinth — all three of its base corners inside
    /// the ground diamond.
    ///
    /// The cluster is hand-placed, and a tower whose base overhangs the slab is
    /// the one error in an isometric drawing that cannot be seen at icon size:
    /// it reads as a tower that is merely *further away*, right up until someone
    /// looks at the splash at 168 px.
    #[test]
    fn every_tower_stands_on_the_plinth() {
        // |dx| / half + |dy| / rise <= 1 is the diamond, and the plinth is a
        // diamond because it is a square at `ISO`.
        let on_slab = |x: f32, y: f32| {
            let dx = (x - mark::PLINTH_C[0]).abs() / mark::PLINTH_HALF;
            let dy = (y - mark::PLINTH_C[1]).abs() / (mark::PLINTH_HALF * mark::ISO);
            dx + dy <= 1.0
        };
        for (i, t) in mark::TOWERS.iter().enumerate() {
            let base = t.y + t.h;
            for (name, x, y) in [
                ("front", t.x, base),
                ("left", t.x - t.wl, base - t.wl * mark::ISO),
                ("right", t.x + t.wr, base - t.wr * mark::ISO),
            ] {
                assert!(
                    on_slab(x, y),
                    "tower {i}'s {name} corner ({x}, {y}) hangs off the plinth"
                );
            }
        }
    }

    /// Nothing in the table reaches outside the tile it is drawn in. The mark is
    /// painted with no clip of any kind — a `Painter` at the sign-in card's 22 pt
    /// will happily draw a tower across the card's own text.
    #[test]
    fn the_cluster_stays_inside_the_tile() {
        let square = 2.0 * mark::UNIT;
        let check = |what: &str, x: f32, y: f32| {
            assert!(
                (0.0..=square).contains(&x) && (0.0..=square).contains(&y),
                "{what} lands at ({x}, {y}), outside the tile"
            );
        };
        for (i, t) in mark::TOWERS.iter().enumerate() {
            check(
                &format!("tower {i} left"),
                t.x - t.wl,
                t.y - t.wl * mark::ISO,
            );
            check(&format!("tower {i} right"), t.x + t.wr, t.y + t.h);
        }
        check(
            "the plinth",
            mark::PLINTH_C[0] - mark::PLINTH_HALF,
            mark::PLINTH_C[1] + mark::PLINTH_HALF * mark::ISO + mark::PLINTH_DEPTH,
        );
        check(
            "the pin",
            mark::PIN_C[0] - mark::PIN_R - mark::PIN_KEYLINE,
            mark::PIN_C[1] - mark::PIN_R - mark::PIN_KEYLINE,
        );
    }

    /// The brand family must be bound before anything paints a heading —
    /// `install` exists to guarantee that, and an unbound family is a panic deep
    /// in epaint rather than a missing-font fallback.
    #[test]
    fn install_binds_the_brand_family_for_the_first_frame() {
        let ctx = egui::Context::default();
        install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let _ = ctx.run_ui(
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
        let out = ctx.run_ui(
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
        // The tile, the shadow, the plinth and its grid, seven towers of two
        // faces and a roof each, the pin, and the wordmark's two galleys.
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
            5 => P::Assistant,
            6 => P::Chat,
            7 => P::Appearance,
            8 => P::Script,
            9 => P::Models,
            10 => P::Settings,
            11 => P::About,
            _ => return None,
        };
        let back = match panel {
            P::None => 0,
            P::Search => 1,
            P::Layers => 2,
            P::Places => 3,
            P::Route => 4,
            P::Assistant => 5,
            P::Chat => 6,
            P::Appearance => 7,
            P::Script => 8,
            P::Models => 9,
            P::Settings => 10,
            P::About => 11,
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
        let _ = ctx.run_ui(
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
             which is a grey since the UI went monochrome"
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
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                let font = TextStyle::Body.resolve(ui.style());
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
                        let drawable = ui.ctx().fonts_mut(|f| f.has_glyphs(&font, &glyph));
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
            let out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ui| {
                    let p = ui.painter();
                    p.rect_filled(screen, 0.0, SURFACE);
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
