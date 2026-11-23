use super::*;

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
pub(super) const BRAND_FAMILY: &str = "cartopolis-brand";

/// The body face: Plus Jakarta Sans **Regular**, the brand face's own family
/// one weight down, at the front of egui's `Proportional` stack.
///
/// **Until 2026-10-04 body text was egui's default, Ubuntu Light**, which is
/// a fine debugging face and a poor one for this UI: a light weight in light
/// ink on a dark panel thins out at the 11–12.5 pt the panels are full of,
/// and next to a Jakarta heading it was a second, unrelated typeface. One
/// family at two weights is what makes a heading and its rows read as one
/// voice. Ubuntu stays behind it in the stack for the glyphs Jakarta lacks.
pub const BODY_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/PlusJakartaSans-Regular.ttf"
));
pub(super) const BODY_FAMILY: &str = "cartopolis-body";

/// Noto Emoji, one-colour, Unicode 15, at the **end** of every stack. egui's
/// own `NotoEmoji-Regular` is an old subset that stops at U+1F6C5, so the
/// emoji people actually type in chat (🙂 🕺 🤖 🛒) drew as `◻` — on the phone
/// too. Last, so every glyph something earlier in the stack already drew
/// keeps its look; colour emoji cannot be drawn by epaint at all.
pub(super) const EMOJI_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/NotoEmoji-Regular.ttf"
));
pub(super) const EMOJI_FAMILY: &str = "cartopolis-emoji";

/// The brand face: Plus Jakarta Sans SemiBold, backed by egui's own stack so a
/// glyph it lacks (emoji, arrows, the box-drawing the panels use) still resolves
/// instead of rendering as tofu. Used for headings and the splash wordmark —
/// *not* for body text, which this weight would make heavy and hard to read.
pub fn brand_family() -> FontFamily {
    FontFamily::Name(BRAND_FAMILY.into())
}

pub(super) fn fonts() -> egui::FontDefinitions {
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
pub(super) const ROW_PTS: f32 = 28.0;
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
