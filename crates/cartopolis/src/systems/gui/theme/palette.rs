use super::*;

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
    pub ink: Rgba8,
    /// The ink on an [`Palette::accent`] plate (an engaged disc, a ticked box,
    /// the primary button's label): white in both themes, on a deep teal
    /// (`#2B837E`, 4.5:1). Until 2026-11-10 the plate was the bright
    /// `#5CC6C0` with `#0F1416` on it, and the person found "a teal button
    /// with black text" not nice.
    pub on_accent: Rgba8,
    /// The ink on a status-coloured plate ([`Palette::error`], [`Palette::warn`]
    /// as a fill — an armed confirm, a raised hand): [`Palette::ink`] in the
    /// dark theme, where the status tones are light; white in the light theme,
    /// where they are dark.
    pub on_status: Rgba8,
    /// The surface ramp's bottom above [`Palette::ink`]: control plates (a
    /// button at rest) and the widget table's noninteractive fill. A card in a
    /// panel is [`Palette::card`].
    pub surface: Rgba8,
    /// One step up from [`Palette::surface`] (in the light theme, a step
    /// toward the page): a button under the pointer, striped rows, a
    /// segmented control's chosen card.
    pub surface_raised: Rgba8,
    /// The ramp's far end: hover in a list, open menus over a panel.
    pub surface_hover: Rgba8,
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
    pub line: Rgba8,
    /// The live tone — **teal**, `#2B837E` in both themes (since 2026-11-10;
    /// it was the bright `#5CC6C0` with dark ink on it). Selected segments,
    /// ticked boxes, sliders, the primary button and the plate an engaged
    /// control wears: the one saturated colour in the chrome, so it always
    /// means "this is the live thing". It carries [`Palette::on_accent`]
    /// (white) at 4.5:1, so anything filled with it is labelled in that.
    pub accent: Rgba8,
    /// The accent at the top of its ladder, as a **stroke**: the text cursor,
    /// a text-selection handle, the rim on a *held* control (north-up lock,
    /// caps lock). White in the dark theme, the hub's deep accent text in the
    /// light one.
    pub accent_bright: Rgba8,
    /// The fill an accent plate changes to under the pointer
    /// ([`primary_button_sized`]): a step deeper, so the white label still
    /// reads (5.1:1).
    pub accent_lift: Rgba8,
    /// The rim of an *accent-filled* plate: an engaged disc, a dock disc whose
    /// panel is open. A deeper teal than the plate, so the white glyph inside
    /// would read on it too (5.8:1). The rim of a pressed or hovered *button*
    /// is [`Palette::accent_word`] instead, which clears 3:1 on those fills.
    pub accent_ring: Rgba8,
    /// The accent, pushed *down* — the fill of an accent-filled button while
    /// the pointer is held on it. A press moves the opposite way from a hover,
    /// or the two states are one state. Its label is still
    /// [`Palette::on_accent`], at 5.8:1.
    pub accent_pressed: Rgba8,
    /// The top of the *text* ladder — [`Palette::text`] → [`Palette::cream`]
    /// → this. Links, speaker names, emphasised values, the open menu entry.
    /// White in the dark theme (emphasis is lightness there); the hub's deep
    /// teal link colour in the light one.
    pub accent_text: Rgba8,
    /// Words in the accent on a soft accent tag ([`Palette::accent_muted`]),
    /// the web's `--m-accent-text`: the pale [`Palette::accent_ring`] in the
    /// dark theme, the hub's deep teal in the light one (where the ring is too
    /// light to be read as type on the tag).
    pub accent_word: Rgba8,
    /// The soft half of the accent: a pressed button, a selected row, a text
    /// selection, an "on" tag. Dark, a deep teal that carries
    /// [`Palette::cream`] at 10.5:1; light, the hub's accent-soft.
    ///
    /// This is what keeps the ladder safe. `active.weak_bg_fill` carries a
    /// label drawn in egui's `strong_text_color`, which is
    /// [`Palette::cream`]; filling it with [`Palette::accent`] instead would
    /// put a light label on a light plate.
    pub accent_muted: Rgba8,
    /// The strongest text: headings and the one or two things that must read
    /// brightest (dark) or darkest (light). egui's `strong_text_color`.
    pub cream: Rgba8,
    /// Body text.
    pub text: Rgba8,
    /// Secondary text: units, coordinates, hints. Also `index.html`'s muted
    /// tone in the dark theme, so the pre-wasm splash and this one agree —
    /// **change it in both**.
    ///
    /// Lifted on 2026-08-14: this is the colour egui hands to
    /// `weak_text_color`, i.e. the one that lands on every surface in the ramp,
    /// and it measured 4.40:1 on the hover step and 4.12:1 inside an overlay box
    /// over bright map imagery. Both now clear 4.5.
    pub text_dim: Rgba8,
    /// Warnings. **One of the two colours the chrome keeps** besides the
    /// accent, for the reason road signs are not brand-coloured: caution and
    /// fault are statements about the world, not decoration. A desaturated gold
    /// on the dark theme, the hub's amber-brown on the light.
    pub warn: Rgba8,
    /// Errors. Warm rather than pure red so it belongs to the same palette;
    /// lifted from `#E88B7D` on 2026-08-14 (dark), because an error is the one
    /// string that must be readable wherever it lands.
    pub error: Rgba8,
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
    pub overlay: Rgba8,
    /// The window and popup fill: [`Palette::overlay`]'s tone. The name stays
    /// for the call sites that paint "the card a window is".
    pub glass: Rgba8,
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
    pub panel: Rgba8,
    /// A card inside a panel ([`list_group`], a chat or contact row). Dark:
    /// just off [`Palette::panel`], so a group of rows reads as one block by
    /// its hairline and shadow rather than by a jump in tone. Light: white.
    pub card: Rgba8,
    /// The hairline round a panel, a window or a box over the map: the midas
    /// hub's `--m-border`, solid. The shadow does the lifting; this only keeps
    /// a card's edge.
    pub panel_rim: Rgba8,
    /// The rim around a box or a round button over the map:
    /// [`Palette::panel_rim`]. It never identifies a *control* — that is
    /// [`Palette::line`]'s job.
    pub overlay_line: Rgba8,
    /// The fill of a control that is a *guide* rather than a plate — the
    /// thumbstick's well, which at [`Palette::overlay`]'s opacity read as a hole
    /// punched in the map rather than as something to put a thumb on. Half the
    /// coverage; nothing legible sits on it. The floor is the other direction:
    /// at alpha 90 the well vanished into pale beige map.
    pub overlay_soft: Rgba8,
    /// The rule between the rows of a [`list_group`]: the text tone at a low
    /// alpha, so it is a hairline on any card.
    pub list_line: Rgba8,
    /// How dark the shadows are, as a factor on their alpha: a light page
    /// takes the hub's faint lift, not the dark theme's deep one.
    pub shadow: f32,
    /// The fill of a tile or card that is *on* (a layer tile): the soft teal
    /// in the dark theme; white in the light one, where the soft teal read
    /// as washed out (2026-11-10) and the accent rim and icon say "on".
    pub tile_on: Rgba8,
    /// The knob of a switch that is on, over its accent track: dark ink in
    /// the dark theme, white in the light one, as a light UI draws a switch.
    pub knob_on: Rgba8,
    /// A count on a round button (the layer categories): the midas hub's gold,
    /// its one accent besides the teal, with [`Palette::on_gold`] on it. It was
    /// a cream or near-black disc, which read as a black dot (2026-11-10).
    pub gold: Rgba8,
    /// The ink on [`Palette::gold`].
    pub on_gold: Rgba8,
    /// The hairline round a button or a field at rest: the midas hub's soft
    /// border, a step firmer. It was [`Palette::line`], a 3:1 grey, and a page
    /// of buttons in it read as "cheap" boxes (2026-11-10); a control is now
    /// told by its plate, its shape and its word, as in the hub, with
    /// [`Palette::line`] kept for what must be found by its edge alone.
    pub control_edge: Rgba8,
    /// The plate of a round button over the map at rest ([`map_disc`],
    /// [`round_button`]), the search pill and the World | Community track.
    /// Dark: [`Palette::overlay`], as it was. Light: white, as the midas hub's
    /// light buttons are.
    pub map_fill: Rgba8,
    /// The glyph on that plate. Dark: [`Palette::text`]. Light: the deep
    /// accent teal (2026-11-10, "the light buttons have no teal theme at
    /// all": they were black glyphs on white discs, and teal was left only on
    /// the one plate that was on).
    pub map_ink: Rgba8,
    /// The rim round that plate. Dark: [`Palette::overlay_line`]. Light: a
    /// faint teal, the brand's edge rather than a grey one.
    pub map_rim: Rgba8,
    /// A strong mark on the map's chrome that is not a glyph — the zoom
    /// rail's thumb. Dark: [`Palette::cream`]. Light: the accent.
    pub map_mark: Rgba8,
    /// Whether egui's dark visuals are the base ([`visuals`]).
    pub dark: bool,
}

/// Today's look: the deep teal, read off Android's keyboard in its teal dark
/// theme (2026-10-10) and laid out as the midas hub lays out its dark mode.
pub const DARK: Palette = Palette {
    ink: Rgba8::from_rgb(0x0F, 0x14, 0x16),
    on_accent: Rgba8::WHITE,
    on_status: Rgba8::from_rgb(0x0F, 0x14, 0x16),
    surface: Rgba8::from_rgb(0x14, 0x1A, 0x1C),
    surface_raised: Rgba8::from_rgb(0x1F, 0x28, 0x29),
    surface_hover: Rgba8::from_rgb(0x2A, 0x35, 0x36),
    line: Rgba8::from_rgb(0x63, 0x79, 0x7C),
    accent: Rgba8::from_rgb(0x2B, 0x83, 0x7E),
    accent_bright: Rgba8::WHITE,
    accent_lift: Rgba8::from_rgb(0x27, 0x7A, 0x75),
    accent_ring: Rgba8::from_rgb(0x24, 0x70, 0x6C),
    accent_pressed: Rgba8::from_rgb(0x24, 0x70, 0x6C),
    accent_text: Rgba8::WHITE,
    accent_word: Rgba8::from_rgb(0xA8, 0xDC, 0xD8),
    accent_muted: Rgba8::from_rgb(0x26, 0x3F, 0x42),
    cream: Rgba8::from_rgb(0xF4, 0xF8, 0xF8),
    text: Rgba8::from_rgb(0xE2, 0xE9, 0xEA),
    text_dim: Rgba8::from_rgb(0xAA, 0xC2, 0xC6),
    warn: Rgba8::from_rgb(0xD8, 0xC0, 0x7A),
    error: Rgba8::from_rgb(0xF2, 0xA2, 0x96),
    overlay: Rgba8::from_rgb(0x17, 0x23, 0x24),
    glass: Rgba8::from_rgb(0x17, 0x23, 0x24),
    panel: Rgba8::from_rgb(0x17, 0x23, 0x24),
    card: Rgba8::from_rgb(0x16, 0x1F, 0x21),
    panel_rim: Rgba8::from_rgb(0x2A, 0x35, 0x36),
    overlay_line: Rgba8::from_rgb(0x2A, 0x35, 0x36),
    overlay_soft: Rgba8::from_rgba_unmultiplied(0x14, 0x1A, 0x1C, 140),
    list_line: Rgba8::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 22),
    shadow: 1.0,
    tile_on: Rgba8::from_rgb(0x26, 0x3F, 0x42),
    knob_on: Rgba8::from_rgb(0xE2, 0xE9, 0xEA),
    gold: Rgba8::from_rgb(0xD8, 0xC0, 0x7A),
    on_gold: Rgba8::from_rgb(0x1F, 0x28, 0x29),
    control_edge: Rgba8::from_rgb(0x3A, 0x48, 0x49),
    map_fill: Rgba8::from_rgb(0x17, 0x23, 0x24),
    map_ink: Rgba8::from_rgb(0xE2, 0xE9, 0xEA),
    map_rim: Rgba8::from_rgb(0x2A, 0x35, 0x36),
    map_mark: Rgba8::from_rgb(0xF4, 0xF8, 0xF8),
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
    ink: Rgba8::from_rgb(0xE7, 0xED, 0xED),
    on_accent: Rgba8::WHITE,
    on_status: Rgba8::WHITE,
    surface: Rgba8::WHITE,
    surface_raised: Rgba8::from_rgb(0xF8, 0xFA, 0xFA),
    surface_hover: Rgba8::from_rgb(0xE7, 0xED, 0xED),
    line: Rgba8::from_rgb(0x78, 0x8B, 0x8E),
    accent: Rgba8::from_rgb(0x2B, 0x83, 0x7E),
    accent_bright: Rgba8::from_rgb(0x17, 0x69, 0x6A),
    accent_lift: Rgba8::from_rgb(0x27, 0x7A, 0x75),
    accent_ring: Rgba8::from_rgb(0x24, 0x70, 0x6C),
    accent_pressed: Rgba8::from_rgb(0x24, 0x70, 0x6C),
    accent_text: Rgba8::from_rgb(0x17, 0x69, 0x6A),
    accent_word: Rgba8::from_rgb(0x17, 0x69, 0x6A),
    accent_muted: Rgba8::from_rgb(0xD9, 0xF0, 0xEE),
    cream: Rgba8::from_rgb(0x0F, 0x14, 0x16),
    text: Rgba8::from_rgb(0x1F, 0x28, 0x29),
    text_dim: Rgba8::from_rgb(0x54, 0x65, 0x68),
    warn: Rgba8::from_rgb(0x85, 0x57, 0x00),
    error: Rgba8::from_rgb(0xB3, 0x36, 0x1E),
    overlay: Rgba8::from_rgb(0xF1, 0xF5, 0xF5),
    glass: Rgba8::from_rgb(0xF1, 0xF5, 0xF5),
    panel: Rgba8::from_rgb(0xF1, 0xF5, 0xF5),
    card: Rgba8::WHITE,
    panel_rim: Rgba8::from_rgb(0xDA, 0xE3, 0xE3),
    overlay_line: Rgba8::from_rgb(0xDA, 0xE3, 0xE3),
    overlay_soft: Rgba8::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 150),
    list_line: Rgba8::from_rgba_unmultiplied(0x0F, 0x14, 0x16, 22),
    shadow: 0.6,
    tile_on: Rgba8::WHITE,
    knob_on: Rgba8::WHITE,
    gold: Rgba8::from_rgb(0xE0, 0xBC, 0x5C),
    on_gold: Rgba8::from_rgb(0x1F, 0x28, 0x29),
    control_edge: Rgba8::from_rgb(0xC9, 0xD4, 0xD5),
    map_fill: Rgba8::WHITE,
    map_ink: Rgba8::from_rgb(0x17, 0x69, 0x6A),
    map_rim: Rgba8::from_rgb(0x9F, 0xD1, 0xCD),
    map_mark: Rgba8::from_rgb(0x2B, 0x83, 0x7E),
    dark: false,
};

pub(super) static DARK_PALETTE: Palette = DARK;
pub(super) static LIGHT_PALETTE: Palette = LIGHT;
pub(super) static LIGHT_ON: AtomicBool = AtomicBool::new(false);

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
        ui.style_mut().visuals = visuals_of(&DARK32);
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
pub const BRAND: Rgba8 = Rgba8::from_rgb(0xFD, 0x60, 0x12);

/// A person's **real location** — where their phone is, shared with a private
/// chat (`systems::private_chat`). Blue because every map app draws the phone
/// that way, and never the brand orange, which marks places sent in a chat: the
/// two must not be confused on the map, one being a spot somebody pointed at and
/// the other being where somebody actually is. Used for nothing else.
pub const REAL_LOCATION: Rgba8 = Rgba8::from_rgb(0x4F, 0xB2, 0xFF);

/// **Public, and on the fediverse**: a community's posts and anything that
/// came from another server (`systems::community_posts`). Lavender, the colour
/// the design gave it (`docs/mockups/communities/v4-public-posts.html`), so the
/// public half of a community never reads like its encrypted topics. Used for
/// nothing else.
pub const PUBLIC: Rgba8 = Rgba8::from_rgb(0xB3, 0x9D, 0xFF);

/// **The council**: an item of a municipal council's business on the Plans
/// layer (`systems::plans`), the square on the map and its badge on a card.
/// The design's deep blue (`docs/mockups/plans-and-decisions/v1.png`), away
/// from the brand orange the layer uses for notices still open. Used for
/// nothing else.
pub const COUNCIL: Rgba8 = Rgba8::from_rgb(0x2F, 0x4A, 0x7A);
/// The council square's rim and the council's text on a dark card: the same
/// blue, light enough to read.
pub const COUNCIL_LIGHT: Rgba8 = Rgba8::from_rgb(0x9D, 0xB7, 0xE6);

/// Backing for the small text chips painted straight onto the map — place-pin
/// labels, the drag ghost, a feature's hover card. Darker and more opaque than
/// [`Palette::overlay`] on purpose: these are a few words over an arbitrary tile, so
/// legibility beats letting the map through.
pub const LABEL_CHIP: Rgba8 = Rgba8::from_rgba_unmultiplied(0x0F, 0x14, 0x16, 235);
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
pub const LABEL_HALO: Rgba8 = Rgba8::from_rgba_unmultiplied(0x16, 0x17, 0x19, 190);
/// The stroke inside [`LABEL_HALO`]: a warm off-white, the colour of road paint
/// rather than of paper, and not [`Palette::cream`] — this one lies on tarmac in the
/// world, that one is the chrome's, and the two are free to move apart.
pub const LABEL_INK: Rgba8 = Rgba8::from_rgb(0xF6, 0xF3, 0xEC);

/// A scenario over the map (`systems::scenario`, `docs/mockups/scenarios/`).
/// A region's own colour is the scenario file's (it is data, like a legend);
/// what is here is the frame each mode wears and the inks of a track.
pub mod scenario {
    use super::Rgba8;

    /// Live: an official source only, so the one frame allowed to look like a
    /// warning (v4-whole).
    pub const LIVE: Rgba8 = Rgba8::from_rgb(0xD6, 0x3A, 0x2F);
    pub const REPLAY: Rgba8 = Rgba8::from_rgb(0x4C, 0xAF, 0x6E);
    /// Exercise: the yellow of its stripes, on every screen.
    pub const EXERCISE: Rgba8 = Rgba8::from_rgb(0xE8, 0xC5, 0x47);
    pub const SANDBOX: Rgba8 = Rgba8::from_rgb(0x9B, 0x7B, 0xFF);

    /// A track's line, behind and (dashed) ahead, and its time ticks.
    pub const TRACK: Rgba8 = Rgba8::from_rgb(0xFF, 0xCF, 0x9A);
    /// The soft line under the path behind.
    pub const TRACK_GLOW: Rgba8 = Rgba8::from_rgba_unmultiplied(0xFF, 0xB2, 0x7A, 40);
    /// The cone of where it may go.
    pub const CONE: Rgba8 = Rgba8::from_rgba_unmultiplied(0xFF, 0x7A, 0x4A, 34);
    pub const CONE_EDGE: Rgba8 = Rgba8::from_rgba_unmultiplied(0xFF, 0x9A, 0x6A, 90);
    /// The line between two areas of a figure: dark and thin, so neighbours
    /// in the same band still read as two places.
    pub const AREA_EDGE: Rgba8 = Rgba8::from_rgba_unmultiplied(0x12, 0x18, 0x1A, 150);
    /// The dark band under the light outline of the place being read, so it
    /// holds on a pale fill as on a dark one.
    pub const AREA_HALO: Rgba8 = Rgba8::from_rgba_unmultiplied(0x0F, 0x14, 0x16, 170);
    /// The glow round a group's head, and its members.
    pub const HEAD: Rgba8 = Rgba8::from_rgba_unmultiplied(0xFF, 0x6A, 0x3A, 90);
    pub const HEAD_DOT: Rgba8 = Rgba8::from_rgb(0xFF, 0xF1, 0xC9);
    /// The rim of a group's label chip.
    pub const HEAD_RIM: Rgba8 = Rgba8::from_rgb(0xFF, 0x8A, 0x5A);
    /// Where some were brought down.
    pub const INTERCEPT: Rgba8 = Rgba8::from_rgb(0x9F, 0xD3, 0xFF);
}
