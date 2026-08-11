//! The Cartopolis look: one palette, one `egui::Style`, one place to change them.
//!
//! A cool slate-blue gradient from `#1B2830` up to `#324855` carries the chrome,
//! the accent orange `#FD6012` carries every live thing, an amber `#FD9212` is
//! the second accent, and `#F2EFEA` is the off-white. So the boot splash hands
//! over to a UI painted in the tones it was already showing.
//!
//! **The ground is deliberately desaturated.** The surface ramp began as the
//! logo's teal (`#0C2A33`–`#164652`, hue 194°, ~60 % saturation) and was moved
//! to hue ~203° at ~28 % — bluer, and a good deal greyer. The accent is the only
//! saturated colour in the palette, and it reads as an accent in proportion to
//! how little chroma sits behind it; against a teal that was itself vivid, the
//! orange had to compete rather than pop. The shift was done in OKLCh with
//! **lightness held fixed**, so only hue and chroma moved: every contrast ratio
//! below is within 0.05 of what the teal ramp measured, and none of the tonal
//! structure the widget table depends on changed.
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
//! [`TEXT`] is 11.9:1 on [`SURFACE`], [`TEXT_DIM`] 6.9:1, [`ACCENT_TEXT`] 6.9:1.
//! Plain [`ACCENT`] is 4.9:1 — comfortable for a fill, a rule or a focus ring, and
//! it clears 4.5:1 for text too, but the lighter [`ACCENT_TEXT`] is kept for small
//! type so links and values hold their margin as the accent moves. Anything
//! sitting *on* an accent-orange fill is drawn in [`INK`] (5.9:1) rather than
//! white (3.1:1).
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

// ─── Palette ──────────────────────────────────────────────────────────────────

/// Deeper than any of the surface tones: text-edit wells, code blocks, and the
/// boot backdrop. Also the text colour on an accent-orange fill.
pub const INK: Color32 = Color32::from_rgb(0x10, 0x18, 0x1E);
/// The surface ramp's bottom. Panel and window fill — the app's base surface.
pub const SURFACE: Color32 = Color32::from_rgb(0x1B, 0x28, 0x30);
/// The ramp's middle. One step up from [`SURFACE`]: buttons at rest,
/// striped rows, anything that should read as sitting *on* a panel.
pub const SURFACE_RAISED: Color32 = Color32::from_rgb(0x26, 0x38, 0x42);
/// The ramp's top. Hover, and open menus over a panel.
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x32, 0x48, 0x55);
/// Hairlines: separators, widget frames, indent guides.
pub const LINE: Color32 = Color32::from_rgb(0x3C, 0x55, 0x64);

/// The accent. Focus rings, hover outlines, sliders, the pressed state — the one
/// saturated colour in the UI, so it always means "this is the live thing".
/// Prefer [`ACCENT_TEXT`] for small type (this is 4.9:1 on [`SURFACE`]).
pub const ACCENT: Color32 = Color32::from_rgb(0xFD, 0x60, 0x12);
/// The accent, lifted — the outline around an active widget's orange fill.
pub const ACCENT_BRIGHT: Color32 = Color32::from_rgb(0xFF, 0x84, 0x3C);
/// The accent as *text*, lightened to hold its margin on [`SURFACE`] (6.9:1).
/// Links, speaker names, emphasised values.
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0xFF, 0x94, 0x57);
/// The accent mixed roughly halfway into the panel: a selected row that still
/// reads as orange while leaving [`CREAM`] on it at 5.8:1.
pub const ACCENT_MUTED: Color32 = Color32::from_rgb(0x96, 0x44, 0x1C);
/// The second accent — reserved for *held* states (north-up lock,
/// follow-and-rotate), so they never collide with [`ACCENT`].
pub const AMBER: Color32 = Color32::from_rgb(0xFD, 0x92, 0x12);

/// Off-white. Headings and the one or two things that must read brightest.
pub const CREAM: Color32 = Color32::from_rgb(0xF2, 0xEF, 0xEA);
/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(0xDF, 0xE5, 0xE7);
/// Secondary text: units, coordinates, hints. Also `index.html`'s muted tone,
/// so the pre-wasm splash and this one agree.
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xA8, 0xB1, 0xBA);
/// Warnings — a desaturated gold that reads as caution without competing with
/// [`AMBER`] or [`ACCENT`].
pub const WARN: Color32 = Color32::from_rgb(0xD8, 0xC0, 0x7A);
/// Errors. Warm rather than pure red so it belongs to the same palette.
pub const ERROR: Color32 = Color32::from_rgb(0xE8, 0x8B, 0x7D);

/// [`SURFACE`] at ~84% — the fill of the boxes that float *over* the map
/// (status HUD, stats, bottom bar). Translucent so the world still moves behind
/// them; opaque enough that the text on top never has to fight a bright tile.
pub const OVERLAY: Color32 = Color32::from_rgba_unmultiplied_const(0x1B, 0x28, 0x30, 214);
/// The hairline around an [`OVERLAY`] box: enough to separate it from the map,
/// not enough to draw a frame around the view.
pub const OVERLAY_LINE: Color32 = Color32::from_rgba_unmultiplied_const(0x3C, 0x55, 0x64, 190);
/// Backing for the small text chips painted straight onto the map — place-pin
/// labels, the drag ghost, a feature's hover card. Darker and more opaque than
/// [`OVERLAY`] on purpose: these are a few words over an arbitrary tile, so
/// legibility beats letting the map through.
pub const LABEL_CHIP: Color32 = Color32::from_rgba_unmultiplied_const(0x10, 0x18, 0x1E, 235);
/// Halo behind map text that has no chip to sit on — the street names
/// (`systems::street_labels`), which are painted *along* their street and would
/// black it out under a chip. Near-white and only mostly opaque: it has to lift
/// [`INK`] off tarmac, grass and water without reading as a second word.
pub const LABEL_HALO: Color32 = Color32::from_rgba_unmultiplied_const(0xF6, 0xF4, 0xF0, 205);

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
/// ([`primary_button`]). One per box — a second is just body text.
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
pub const RADIUS: u8 = 6;
/// Small: the checkbox square, the close button, a label chip painted over the
/// map. For anything under ~24 pt, where [`RADIUS`] is half the height and the
/// box stops reading as a box (an unchecked checkbox becomes a radio button).
pub const RADIUS_SM: u8 = 3;
/// Large: windows, cards, and the boxes that float over the map
/// ([`overlay_frame`]). A bigger surface needs a bigger radius to look equally
/// rounded — the same value at two sizes does not.
pub const RADIUS_LG: u8 = 10;
/// A pill: fully rounded whatever the height. epaint clamps a corner radius to
/// half the shortest side, so this is "as round as it can be" rather than a
/// pixel count — which is what makes it safe on a box of any size.
#[allow(dead_code)] // vocabulary, no caller yet — see the note by `CHIP_PADDING`
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
#[allow(dead_code)] // read by `min_touch_target_pts` and the tests until the widgets convert
pub const MIN_TOUCH_TARGET_DP: f32 = 48.0;

/// `index.html`'s `MAX_DPR` on the phone tier — device pixels per CSS pixel the
/// render budget will draw (`docs/notes/wasm-performance.md`). It matters here
/// because it is *also* the magnification the stage transform applies to every
/// egui point on the web, i.e. the thing that decides how big a point physically
/// is. Duplicated rather than plumbed, because the page decides it before any
/// Rust exists; if it changes there, [`dp_per_point`] changes with it.
#[allow(dead_code)] // ditto — `onscreen_keyboard.rs` still carries its own copy
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
#[allow(dead_code)] // the copy in `onscreen_keyboard.rs` converts to this
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
#[allow(dead_code)] // no caller until the driver rail and the keyboard convert
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
        // Hover is where the fox first appears: the surface lifts a step and
        // gains an orange outline, so "clickable" is a colour, not a guess.
        hovered: WidgetVisuals {
            expansion: 1.0,
            ..visuals(SURFACE_HOVER, ACCENT, CREAM)
        },
        // Pressed / focused. Two different oranges on purpose:
        //   - `bg_fill` is the *graphical* fill (slider grab, checkbox box), so it
        //     gets the full fox;
        //   - `weak_bg_fill` is the button fill, which carries a label, so it gets
        //     the muted one — [`CREAM`] is 5.7:1 on that and only 3.1:1 on [`ACCENT`].
        // `fg_stroke` must stay bright regardless: egui derives
        // `Visuals::strong_text_color` from it, so *every* `RichText::strong()` in
        // the app is this colour. Setting it to ink to suit an orange fill turned
        // every bold value in the UI near-invisible.
        active: WidgetVisuals {
            weak_bg_fill: ACCENT_MUTED,
            expansion: 1.0,
            ..visuals(ACCENT, ACCENT_BRIGHT, CREAM)
        },
        // An open menu button keeps the hover surface but drops the orange ring —
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

    v.panel_fill = SURFACE;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.window_corner_radius = CornerRadius::same(RADIUS_LG);
    v.menu_corner_radius = CornerRadius::same(RADIUS);
    // Softer and further than egui's default: these float over a moving map, and
    // a tight hard shadow reads as a sticker rather than a raised surface.
    v.window_shadow = Shadow {
        offset: [0, 12],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(110),
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

// ─── Logo ─────────────────────────────────────────────────────────────────────

/// Side of the logo texture, in texels. Drawn at [`LOGO_SIZE`] points, so this is
/// ~2× for a hidpi screen; egui has no mip chain, and the 256 px source scaled to
/// 26 pt in the sampler shimmers as the window moves under it.
const LOGO_TEXELS: u32 = 64;

/// The address bar's logo button, in points.
pub const LOGO_SIZE: f32 = 26.0;

/// The app mark as an egui image, decoded from the same embedded PNG the OS
/// window icon and the XDG desktop entry use — one asset, three surfaces.
///
/// Returns `None` only if that PNG fails to decode, which in practice means a
/// clone without Git LFS (the pointer file is 130 bytes of text). The bar then
/// falls back to a text mark rather than panicking, because a broken checkout
/// should still boot far enough to say so.
pub fn logo_image() -> Option<egui::ColorImage> {
    let png = crate::systems::window_icon::ICON_PNG;
    let img = match image::load_from_memory(png) {
        Ok(img) => img,
        Err(e) => {
            tracing::warn!(error = %e, "logo: embedded icon PNG did not decode (Git LFS?)");
            return None;
        }
    };
    let small = image::imageops::resize(
        &img.into_rgba8(),
        LOGO_TEXELS,
        LOGO_TEXELS,
        image::imageops::FilterType::Lanczos3,
    );
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [LOGO_TEXELS as usize; 2],
        small.as_raw(),
    ))
}

// ─── Glyphs ───────────────────────────────────────────────────────────────────

/// The first of `candidates` the loaded font can actually draw.
///
/// egui bundles Ubuntu-Light, NotoEmoji-Regular and emoji-icon-font, and a
/// character in none of the three renders as an empty box. The union is thinner
/// than it looks *and* wider than the guesses people make about it, which is why
/// this is a probe rather than a list anyone maintains by eye.
///
/// The inventory below was re-derived from the four cmaps in the `Proportional`
/// stack on 2026-08-10, and it corrected the previous one in both directions:
///
///   - **Missing** (draws as tofu): `⇅ ⬤ ← → ↑ ↓ ↰ ↱ ✕ ✓ 🖈 ● ▲ ▼ ≡ ⌫ ⏎ ⇧ 🧭 🤖 🧍 ⧉ 🪑`.
///   - **Present**, contrary to the note that used to stand here: `📍 📋 ⎘ ⬆ ⬅ ➡ ⊞
///     ✔ ☆ ★ 🏠 ☀ 🌙 ☰ ⏵ ⏴ ⏶ ⏷ ⌖ ◎ ○ • ⚑ ↻ ⟲ ▶ ◀ ⚙ 🔍 💬 🗺 📌 …`. An
///     over-conservative inventory is not free: it sends the next person down a
///     fallback ladder for a glyph the font has, and `📍` — the obvious mark for
///     a place pin — spent that whole time declared unavailable.
///
/// So no icon in the app is hard-coded to a guess: each is picked from a
/// preference list whose last entry is Latin-1 and therefore cannot fail. The
/// test `no_painted_glyph_is_tofu` walks the source and holds every call site to
/// that rule.
///
/// Lives here rather than beside any one panel because the answer depends on
/// [`fonts`] — change the font stack and every icon list has to be re-probed
/// against it, from one place.
pub fn first_drawable(ctx: &egui::Context, candidates: &[&'static str]) -> &'static str {
    let font = egui::TextStyle::Body.resolve(&ctx.style_of(ctx.theme()));
    ctx.fonts_mut(|f| {
        candidates
            .iter()
            .copied()
            .find(|c| f.has_glyphs(&font, c))
            .unwrap_or_else(|| candidates.last().copied().unwrap_or("?"))
    })
}

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
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn chip_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(LABEL_CHIP)
        .corner_radius(CornerRadius::same(RADIUS_SM))
        .inner_margin(Margin::symmetric(
            CHIP_PADDING.x as i8,
            CHIP_PADDING.y as i8,
        ))
}

/// Smallest width of a primary button, in points — wide enough that a two-letter
/// label ("Go") is still a button rather than a square.
#[allow(dead_code)]
pub const PRIMARY_BUTTON_MIN_WIDTH: f32 = 120.0;
/// Height of a primary button on a pointer. Under a finger it grows to
/// [`row_height`], which is larger.
#[allow(dead_code)]
const PRIMARY_BUTTON_HEIGHT: f32 = 34.0;

/// **The** call to action on a screen: filled with the fox, labelled in [`INK`]
/// at [`SIZE_VALUE`], and taller than an ordinary button.
///
/// There should be at most one per screen, and it should be the thing you came to
/// do — Sign in, Start driving. Everything else is `ui.button`.
///
/// Exists because the app already had two primary buttons that did not look
/// alike: the sign-in CTA was a hand-built filled `Button` and the Route panel's
/// equally-primary "▶ Driver mode" was a plain `ui.button`, so the same rank read
/// as two different ones. `INK` on `ACCENT` and not white is the palette's rule for
/// text on the accent (6.0:1 against 3.1:1) and is unit-tested below.
#[allow(dead_code)]
pub fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    primary_button_sized(ui, label, PRIMARY_BUTTON_MIN_WIDTH)
}

/// [`primary_button`] with the minimum width stated — for a CTA that has to fill
/// the card it sits in.
#[allow(dead_code)]
pub fn primary_button_sized(ui: &mut egui::Ui, label: &str, min_width: f32) -> egui::Response {
    let height = PRIMARY_BUTTON_HEIGHT.max(row_height());
    ui.add(
        egui::Button::new(
            egui::RichText::new(label)
                .size(SIZE_VALUE)
                .strong()
                .color(INK),
        )
        .fill(ACCENT)
        .corner_radius(CornerRadius::same(RADIUS))
        .min_size(egui::vec2(min_width, height)),
    )
}

/// Diameter of a round icon button, in points. Matches the compass disc in the
/// opposite corner — the map's floating controls are one family. Already 60 dp on
/// the touch tier, so it needs no finger-sized variant.
pub const ROUND_BUTTON_SIZE: f32 = 40.0;

/// A round icon button that floats over the map: the dock along the bottom, and
/// anything else that wants the compass's visual language.
///
/// Painted rather than assembled from `Button`, because egui's button is a
/// rounded *rect* whose corner radius is capped well below half its height — a
/// "circular" button built that way is a squircle that reads as a mistake next to
/// the genuinely round compass. `selected` is the open-panel state and takes the
/// fox fill, the same "this is on" colour the rest of the UI uses.
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
        (true, _) => (ACCENT_MUTED, ACCENT_BRIGHT, CREAM),
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
        },
    );
    resp.on_hover_text(tooltip)
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
}

fn paint_disc(p: &egui::Painter, center: egui::Pos2, radius: f32, d: Disc<'_>) {
    p.circle_filled(center, radius, d.fill);
    p.circle_stroke(center, radius, Stroke::new(1.0, d.ring));
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

/// The ✕ every panel closes with — **painted, not typed.**
///
/// `✕` is not in the bundled font (see [`first_drawable`]), so every panel that
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
        (ACCENT_MUTED, ACCENT_BRIGHT, CREAM)
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
    resp.on_hover_text("Close")
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
pub fn panel_header(ui: &mut egui::Ui, icons: &[&'static str], title: &str) -> bool {
    panel_header_with(ui, icons, title, |_| {})
}

/// [`panel_header`] plus room for extra actions, laid out right-to-left from the
/// close button (the Places panel's "+ Folder").
pub fn panel_header_with(
    ui: &mut egui::Ui,
    icons: &[&'static str],
    title: &str,
    extra: impl FnOnce(&mut egui::Ui),
) -> bool {
    let icon = first_drawable(ui.ctx(), icons);
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

    let mut close = false;
    {
        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(band.shrink2(egui::Vec2::splat(HEADER_INSET)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row.spacing_mut().item_spacing.x = 8.0;
        let (medallion, _) =
            row.allocate_exact_size(egui::Vec2::splat(HEADER_MEDALLION), egui::Sense::hover());
        paint_disc(
            row.painter(),
            medallion.center(),
            HEADER_MEDALLION * 0.5 - 0.5,
            Disc {
                fill: ACCENT_MUTED,
                ring: ACCENT_BRIGHT,
                ink: CREAM,
                icon,
                icon_size: 13.0,
            },
        );
        row.add(
            egui::Label::new(
                egui::RichText::new(title)
                    .font(FontId::new(SIZE_TITLE, brand_family()))
                    .color(CREAM),
            )
            .truncate()
            .selectable(false),
        );
        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = close_button(ui).clicked();
            extra(ui);
        });
    }
    ui.advance_cursor_after_rect(band);
    // Clears the shadow, which reaches below the band.
    ui.add_space(4.0);
    close
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
            ("profile initial on the accent", INK, ACCENT, 4.5),
            ("warning on panel", WARN, SURFACE, 4.5),
            ("error on panel", ERROR, SURFACE, 4.5),
            // Not text: a fill, a rule or a focus ring, which WCAG puts at 3:1.
            ("focus ring on panel", ACCENT, SURFACE, 3.0),
        ] {
            let got = contrast(fg, bg);
            assert!(got >= min, "{name}: contrast {got:.2} < {min}");
        }
    }

    /// egui derives three app-wide text colours from the widget table rather
    /// than exposing them directly — plain text from `noninteractive`, weak text
    /// from `weak_text_color`, and **`RichText::strong()` from `widgets.active`**.
    /// That last one is the trap: `active` is also the pressed-button state, so
    /// picking its `fg_stroke` to suit an orange fill silently recolours every
    /// bold value in the app. Assert the three land where the palette says.
    #[test]
    fn derived_text_colours_come_from_the_palette() {
        let v = visuals();
        assert_eq!(v.text_color(), TEXT);
        assert_eq!(v.weak_text_color(), TEXT_DIM);
        assert_eq!(v.strong_text_color(), CREAM);
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
            1 => P::Layers,
            2 => P::Places,
            3 => P::Route,
            4 => P::Assistant,
            5 => P::Chat,
            6 => P::Appearance,
            7 => P::Script,
            8 => P::Settings,
            9 => P::About,
            _ => return None,
        };
        let back = match panel {
            P::None => 0,
            P::Layers => 1,
            P::Places => 2,
            P::Route => 3,
            P::Assistant => 4,
            P::Chat => 5,
            P::Appearance => 6,
            P::Script => 7,
            P::Settings => 8,
            P::About => 9,
        };
        assert_eq!(back, i, "panel_at's two tables disagree at {i}");
        Some(panel)
    }

    /// No panel may wear a glyph the font cannot draw. [`first_drawable`] only
    /// promises to fall back to the *last* candidate, so a list whose tail is
    /// itself missing (`🧭`, `🤖`, `🧍` all were) still renders an empty box — and
    /// now that the header repeats the dock's icon, one bad list draws tofu twice.
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
                    let icon = first_drawable(ui.ctx(), panel.icons());
                    let font = TextStyle::Body.resolve(ui.style());
                    assert!(
                        ui.ctx().fonts_mut(|f| f.has_glyphs(&font, icon)),
                        "{}: resolved to {icon:?}, which the font cannot draw",
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
    /// [`first_drawable`] preference lists already accounted for.
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

        // A bracketed run of nothing but string literals is a `first_drawable`
        // preference list, and its whole point is that the earlier entries may be
        // missing — only the last one has to resolve. Recognising that here is
        // what keeps the test from flagging the very mechanism that fixes it.
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
    /// [`first_drawable`] exists to guarantee this and is used at six sites
    /// against some thirty hard-coded glyphs, so the guarantee held wherever
    /// somebody remembered it. Two characters were shipping as empty boxes when
    /// this was written — the HUD's `⧉` copy button and the furniture prompt's
    /// `🪑` — and neither is visible in a review, a `cargo check` or a screenshot
    /// nobody happens to take of that corner.
    ///
    /// So the test does what a reviewer cannot: it reads the source, takes every
    /// short string literal that is not part of a preference list, and asks the
    /// *loaded font stack* whether each non-Latin-1 character in it has a glyph.
    /// Latin-1 is exempt because Ubuntu-Light covers it and a `°` or a `·` is not
    /// a guess anyone is making.
    #[test]
    fn no_painted_glyph_is_tofu() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        source_files(&root, &mut files);
        assert!(
            files.len() > 20,
            "found only {} source files under {} — the walk is broken, not the tree",
            files.len(),
            root.display()
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
                    "the font cannot draw these, so they paint as empty boxes — pick them \
                     with `theme::first_drawable` from a list ending in something Latin-1:\n  {}",
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
