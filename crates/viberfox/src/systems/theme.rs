//! The Viberfox look: one palette, one `egui::Style`, one place to change them.
//!
//! The colours are read off the logo (`assets/icon/viberfox.png`) rather than
//! invented — a teal gradient from `#0C2A33` up to `#164652`, the fox at
//! `#FD6012` with an amber `#FD9212` in its fur, and the muzzle's off-white
//! `#F2EFEA`. So the app's chrome and its icon are the same two colours, and the
//! boot splash hands over to a UI painted in the tones it was already showing.
//!
//! Two things this replaces, both of which the UI used to get by default:
//!
//!   - **egui's stock dark theme**, which is a neutral grey (`gray(27)` panels)
//!     with a blue selection. Nothing about it said Viberfox, and next to the
//!     map — which is bright — flat grey read as a hole rather than a surface.
//!   - **egui's stock spacing**, which is tuned for a debug inspector: 3 pt
//!     between rows and 1 pt of vertical button padding. In a panel of place
//!     rows and layer toggles that is a wall of text; the values here roughly
//!     double the vertical rhythm and give buttons a real hit area.
//!
//! Contrast was checked against the surface each colour actually lands on:
//! [`TEXT`] is 8.6:1 on [`SURFACE`], [`TEXT_DIM`] 5.2:1, [`FOX_TEXT`] 5.2:1.
//! Plain [`FOX`] is only 3.7:1, which is fine for a fill, a rule or a focus ring
//! but not for small text — that is why there is a separate, lighter [`FOX_TEXT`]
//! and why anything sitting *on* a fox-orange fill is drawn in [`INK`] (6.0:1)
//! rather than white (3.1:1).
//!
//! [`install`] is called once, from `EguiManager::default`, before the first
//! pass — early enough that the brand font family is bound on frame one, which
//! is what lets the splash paint its wordmark immediately.

use egui::{
    style::{Selection, WidgetVisuals, Widgets},
    Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
};

// ─── Palette ──────────────────────────────────────────────────────────────────

/// Deeper than any of the logo's tones: text-edit wells, code blocks, and the
/// boot backdrop. Also the text colour on a fox-orange fill.
pub const INK: Color32 = Color32::from_rgb(0x07, 0x1A, 0x20);
/// The logo gradient's bottom. Panel and window fill — the app's base surface.
pub const SURFACE: Color32 = Color32::from_rgb(0x0C, 0x2A, 0x33);
/// The logo gradient's middle. One step up from [`SURFACE`]: buttons at rest,
/// striped rows, anything that should read as sitting *on* a panel.
pub const SURFACE_RAISED: Color32 = Color32::from_rgb(0x12, 0x3B, 0x46);
/// The logo gradient's top. Hover, and open menus over a panel.
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x18, 0x4C, 0x5A);
/// Hairlines: separators, widget frames, indent guides.
pub const LINE: Color32 = Color32::from_rgb(0x1F, 0x5A, 0x69);

/// The fox. Focus rings, hover outlines, sliders, the pressed state — the one
/// saturated colour in the UI, so it always means "this is the live thing".
/// Not for small text (3.7:1 on [`SURFACE`]); use [`FOX_TEXT`].
pub const FOX: Color32 = Color32::from_rgb(0xFD, 0x60, 0x12);
/// The fox, lifted — the outline around an active widget's orange fill.
pub const FOX_BRIGHT: Color32 = Color32::from_rgb(0xFF, 0x84, 0x3C);
/// The fox as *text*, lightened until it clears 4.5:1 on [`SURFACE`]. Links,
/// speaker names, emphasised values.
pub const FOX_TEXT: Color32 = Color32::from_rgb(0xFF, 0x94, 0x57);
/// The fox mixed roughly halfway into the panel: a selected row that still
/// reads as orange while leaving [`CREAM`] on it at 5.7:1.
pub const FOX_MUTED: Color32 = Color32::from_rgb(0x96, 0x44, 0x1C);
/// The amber in the logo's fur. The second accent — reserved for *held* states
/// (north-up lock, follow-and-rotate), so they never collide with the fox.
pub const AMBER: Color32 = Color32::from_rgb(0xFD, 0x92, 0x12);

/// The muzzle. Headings and the one or two things that must read brightest.
pub const CREAM: Color32 = Color32::from_rgb(0xF2, 0xEF, 0xEA);
/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(0xDC, 0xE6, 0xE8);
/// Secondary text: units, coordinates, hints. Also `index.html`'s muted tone,
/// so the pre-wasm splash and this one agree.
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9F, 0xB3, 0xBF);
/// Warnings — a desaturated gold that reads as caution without competing with
/// [`AMBER`] or the fox.
pub const WARN: Color32 = Color32::from_rgb(0xD8, 0xC0, 0x7A);
/// Errors. Warm rather than pure red so it belongs to the same palette.
pub const ERROR: Color32 = Color32::from_rgb(0xE8, 0x8B, 0x7D);

/// [`SURFACE`] at ~84% — the fill of the boxes that float *over* the map
/// (status HUD, stats, bottom bar). Translucent so the world still moves behind
/// them; opaque enough that the text on top never has to fight a bright tile.
pub const OVERLAY: Color32 = Color32::from_rgba_unmultiplied_const(0x0C, 0x2A, 0x33, 214);
/// The hairline around an [`OVERLAY`] box: enough to separate it from the map,
/// not enough to draw a frame around the view.
pub const OVERLAY_LINE: Color32 = Color32::from_rgba_unmultiplied_const(0x1F, 0x5A, 0x69, 190);
/// Backing for the small text chips painted straight onto the map — place-pin
/// labels, the drag ghost, a feature's hover card. Darker and more opaque than
/// [`OVERLAY`] on purpose: these are a few words over an arbitrary tile, so
/// legibility beats letting the map through.
pub const LABEL_CHIP: Color32 = Color32::from_rgba_unmultiplied_const(0x07, 0x1A, 0x20, 235);

// ─── Fonts ────────────────────────────────────────────────────────────────────

/// The wordmark face (SIL OFL — see `assets/fonts/README.md`). Embedded rather
/// than loaded as an asset because the splash draws on frame one, before any
/// asset pipeline has run — and on the web that would be an HTTP round trip.
/// egui reads TTF/OTF, not woff2.
const BRAND_TTF: &[u8] = include_bytes!("../../../../assets/fonts/PlusJakartaSans-SemiBold.ttf");
const BRAND_FAMILY: &str = "viberfox-brand";

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

// ─── Style ────────────────────────────────────────────────────────────────────

/// Corner radius shared by buttons, menus and windows. One value, because a UI
/// that rounds three ways reads as three UIs.
const RADIUS: u8 = 6;

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
            ..visuals(SURFACE_HOVER, FOX, CREAM)
        },
        // Pressed / focused. Two different oranges on purpose:
        //   - `bg_fill` is the *graphical* fill (slider grab, checkbox box), so it
        //     gets the full fox;
        //   - `weak_bg_fill` is the button fill, which carries a label, so it gets
        //     the muted one — [`CREAM`] is 5.7:1 on that and only 3.1:1 on [`FOX`].
        // `fg_stroke` must stay bright regardless: egui derives
        // `Visuals::strong_text_color` from it, so *every* `RichText::strong()` in
        // the app is this colour. Setting it to ink to suit an orange fill turned
        // every bold value in the UI near-invisible.
        active: WidgetVisuals {
            weak_bg_fill: FOX_MUTED,
            expansion: 1.0,
            ..visuals(FOX, FOX_BRIGHT, CREAM)
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
        bg_fill: FOX_MUTED,
        stroke: Stroke::new(1.0, CREAM),
    };
    v.hyperlink_color = FOX_TEXT;
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
    v.window_corner_radius = CornerRadius::same(RADIUS + 4);
    v.menu_corner_radius = CornerRadius::same(RADIUS + 2);
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

    v.text_cursor.stroke = Stroke::new(2.0, FOX_BRIGHT);
    // The filled part of a slider takes the accent, so a value reads at a glance.
    v.slider_trailing_fill = true;
    v
}

/// The full brand [`egui::Style`] — [`visuals`] plus the spacing and type scale.
pub fn style() -> egui::Style {
    let mut style = egui::Style {
        visuals: visuals(),
        ..Default::default()
    };

    let s = &mut style.spacing;
    // The "cramped" fix is mostly here. egui's defaults are 3 pt between rows and
    // vec2(4, 1) of button padding; those are inspector values, and a panel of
    // place rows built on them has no rhythm at all.
    s.item_spacing = egui::vec2(8.0, 6.0);
    s.button_padding = egui::vec2(9.0, 4.0);
    // 24 pt of guaranteed height per clickable row (egui ships 18).
    s.interact_size = egui::vec2(40.0, 24.0);
    s.window_margin = Margin::same(10);
    s.menu_margin = Margin::same(8);
    s.menu_spacing = 4.0;
    s.indent = 20.0;
    s.icon_spacing = 6.0;
    s.slider_width = 120.0;
    s.combo_width = 120.0;
    s.scroll.bar_width = 10.0;

    style.text_styles = [
        (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
        // The one place the brand face appears in the app chrome.
        (TextStyle::Heading, FontId::new(19.0, brand_family())),
        (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
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
        .corner_radius(CornerRadius::same(RADIUS + 2))
        .inner_margin(Margin::symmetric(11, 8))
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
            ("link on panel", FOX_TEXT, SURFACE, 4.5),
            ("selected row", CREAM, FOX_MUTED, 4.5),
            ("label on pressed button", CREAM, FOX_MUTED, 4.5),
            ("profile initial on the accent", INK, FOX, 4.5),
            ("warning on panel", WARN, SURFACE, 4.5),
            ("error on panel", ERROR, SURFACE, 4.5),
            // Not text: a fill, a rule or a focus ring, which WCAG puts at 3:1.
            ("focus ring on panel", FOX, SURFACE, 3.0),
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
}
