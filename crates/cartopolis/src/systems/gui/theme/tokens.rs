//! Cartopolis' design tokens as the component library types them: one
//! `Tokens` value, from which the web's `theme.css` is written
//! (`web_theme`, a test) and which the Bevy backend draws with
//! (`mithril_bevy`, behind `ui-probe`). Phase 4 of
//! `docs/mockups/component-library`: the palette existed in the client's
//! [`Palette`] and again, mapped by hand, in the web's stylesheet; this is
//! the one table both now read.
//!
//! **The dark half is the client's palette as the web draws it.** Every dark
//! value is a field of [`WEB`]; none is a fresh hex. **The light half is the
//! web's own** and lives here so it is still in the one place UI colour is
//! chosen, beside the dark value it answers to. The client's own light theme
//! ([`super::LIGHT`], 2026-11-10) took these values, role for role, and is
//! free to move apart from them.

use mithril_style::{Pair, Tokens};

use super::{self as theme, Palette, Rgba8};

/// The client's dark palette as the web still draws it. On 2026-11-10 the
/// app's accent plates went a deeper teal with white ink (the person: "a teal
/// button with black text is not nice"); the web was not part of that ask, so
/// its generated theme keeps the bright accent and dark ink it had — and so,
/// for now, do the library's screens in the app, which draw from this table.
pub(crate) const WEB: Palette = Palette {
    accent: Rgba8::from_rgb(0x5C, 0xC6, 0xC0),
    on_accent: Rgba8::from_rgb(0x0F, 0x14, 0x16),
    accent_lift: Rgba8::WHITE,
    accent_pressed: Rgba8::from_rgb(0x3F, 0xA9, 0xA2),
    accent_ring: Rgba8::from_rgb(0xA8, 0xDC, 0xD8),
    ..theme::DARK
};

/// The font family name the web's `@font-face` rules declare (`app.css`). The
/// files are the client's own, copied into `dist/fonts/` by `build.ts`.
pub(crate) const FONT_FAMILY: &str = "Plus Jakarta Sans";

// ─── The light half ───────────────────────────────────────────────────────────

/// Page background: an off-white with the ramp's teal cast, the light twin of
/// [`theme::INK`].
pub(crate) const L_BG: Rgba8 = Rgba8::from_rgb(0xF1, 0xF5, 0xF5);
pub(crate) const L_SURFACE: Rgba8 = Rgba8::WHITE;
pub(crate) const L_SURFACE_RAISED: Rgba8 = Rgba8::from_rgb(0xF8, 0xFA, 0xFA);
pub(crate) const L_SURFACE_SUNKEN: Rgba8 = Rgba8::from_rgb(0xE7, 0xED, 0xED);
/// Body text is the client's raised surface: the ramp, read the other way.
pub(crate) const L_TEXT: Rgba8 = WEB.surface_raised;
pub(crate) const L_TEXT_DIM: Rgba8 = Rgba8::from_rgb(0x54, 0x65, 0x68);
pub(crate) const L_TEXT_FAINT: Rgba8 = Rgba8::from_rgb(0x84, 0x97, 0x9A);
pub(crate) const L_BORDER: Rgba8 = Rgba8::from_rgb(0xDA, 0xE3, 0xE3);
/// A control's boundary. Clears 3:1 on white for the reason [`theme::LINE`]
/// does on the dark ramp (WCAG 1.4.11): a field at rest has no other edge.
pub(crate) const L_BORDER_STRONG: Rgba8 = Rgba8::from_rgb(0x84, 0x97, 0x9A);
/// Pushed under the pointer: the same move as [`theme::ACCENT_PRESSED`].
pub(crate) const L_ACCENT_HOVER: Rgba8 = WEB.accent_pressed;
/// Links and accent-coloured words on white. The teal itself is too light to
/// read there, so the text tint is a deep version of it.
pub(crate) const L_ACCENT_TEXT: Rgba8 = Rgba8::from_rgb(0x17, 0x69, 0x6A);
/// Selected rows, a pressed nav item: the light twin of [`theme::ACCENT_MUTED`].
pub(crate) const L_ACCENT_SOFT: Rgba8 = Rgba8::from_rgb(0xD9, 0xF0, 0xEE);

// ─── Status tones (the client has two: WARN and ERROR) ───────────────────────

pub(crate) const L_INFO: Rgba8 = Rgba8::from_rgb(0x1F, 0x6A, 0xB0);
pub(crate) const D_INFO: Rgba8 = theme::REAL_LOCATION;
pub(crate) const L_INFO_SOFT: Rgba8 = Rgba8::from_rgb(0xE4, 0xF0, 0xFB);
pub(crate) const D_INFO_SOFT: Rgba8 = Rgba8::from_rgb(0x15, 0x27, 0x36);
pub(crate) const L_SUCCESS: Rgba8 = Rgba8::from_rgb(0x1C, 0x74, 0x42);
pub(crate) const D_SUCCESS: Rgba8 = Rgba8::from_rgb(0x7C, 0xCF, 0x9B);
pub(crate) const L_SUCCESS_SOFT: Rgba8 = Rgba8::from_rgb(0xE3, 0xF4, 0xE9);
pub(crate) const D_SUCCESS_SOFT: Rgba8 = Rgba8::from_rgb(0x15, 0x2C, 0x1E);
pub(crate) const L_WARNING: Rgba8 = Rgba8::from_rgb(0x85, 0x57, 0x00);
pub(crate) const D_WARNING: Rgba8 = WEB.warn;
pub(crate) const L_WARNING_SOFT: Rgba8 = Rgba8::from_rgb(0xFB, 0xF1, 0xDA);
pub(crate) const D_WARNING_SOFT: Rgba8 = Rgba8::from_rgb(0x30, 0x29, 0x16);
pub(crate) const L_DANGER: Rgba8 = Rgba8::from_rgb(0xB3, 0x36, 0x1E);
pub(crate) const D_DANGER: Rgba8 = WEB.error;
pub(crate) const L_DANGER_SOFT: Rgba8 = Rgba8::from_rgb(0xFC, 0xE8, 0xE4);
pub(crate) const D_DANGER_SOFT: Rgba8 = Rgba8::from_rgb(0x36, 0x1D, 0x19);

/// An opaque colour as CSS.
pub(crate) fn hex(c: Rgba8) -> String {
    debug_assert_eq!(c.a(), 255, "a translucent colour goes through alpha()");
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// An opaque colour at a percentage, as CSS. Its own alpha is ignored: the
/// percentage is the alpha. (This took an opaque colour rather than a
/// translucent one while colours were egui's premultiplied `Rgba8`, which
/// does not read back the colour it was made from at 5 %.)
pub(crate) fn alpha(c: Rgba8, pct: f32) -> String {
    format!("rgb({} {} {} / {}%)", c.r(), c.g(), c.b(), pct.round())
}

fn pair(light: Rgba8, dark: Rgba8) -> Pair {
    Pair::new(&hex(light), &hex(dark))
}

fn both(c: Rgba8) -> Pair {
    Pair::same(&hex(c))
}

/// The tokens. Measures the library does not take from here keep its own
/// (`Tokens::garage44()`): type sizes, spacing, control heights, shadows.
pub fn tokens() -> Tokens {
    let w = &WEB;
    Tokens {
        accent: both(w.accent),
        accent_hover: pair(L_ACCENT_HOVER, w.accent_ring),
        // Dark: the pale teal, so words in it read as well on the selected
        // fill ([`theme::ACCENT_MUTED`]) as on a panel.
        accent_text: pair(L_ACCENT_TEXT, w.accent_ring),
        accent_soft: pair(L_ACCENT_SOFT, w.accent_muted),
        on_accent: both(w.on_accent),
        bg: pair(L_BG, w.ink),
        surface: pair(L_SURFACE, w.surface),
        surface_raised: pair(L_SURFACE_RAISED, w.surface_raised),
        surface_sunken: pair(L_SURFACE_SUNKEN, w.ink),
        hover: Pair::new(&alpha(w.ink, 5.0), &alpha(w.cream, 6.0)),
        pressed: Pair::new(&alpha(w.ink, 10.0), &alpha(w.cream, 11.0)),
        text: pair(L_TEXT, w.text),
        text_strong: pair(w.ink, w.cream),
        text_dim: pair(L_TEXT_DIM, w.text_dim),
        text_faint: pair(L_TEXT_FAINT, w.line),
        border: pair(L_BORDER, w.surface_hover),
        border_strong: pair(L_BORDER_STRONG, w.line),
        control_bg: pair(L_SURFACE, w.ink),
        info: pair(L_INFO, D_INFO),
        info_soft: pair(L_INFO_SOFT, D_INFO_SOFT),
        success: pair(L_SUCCESS, D_SUCCESS),
        success_soft: pair(L_SUCCESS_SOFT, D_SUCCESS_SOFT),
        warning: pair(L_WARNING, D_WARNING),
        warning_soft: pair(L_WARNING_SOFT, D_WARNING_SOFT),
        danger: pair(L_DANGER, D_DANGER),
        danger_soft: pair(L_DANGER_SOFT, D_DANGER_SOFT),
        focus: Pair::new(&alpha(w.accent, 50.0), &alpha(w.accent, 55.0)),
        backdrop: Pair::new(&alpha(w.ink, 40.0), &alpha(Rgba8::BLACK, 60.0)),
        // Dark: the midas hub's card shadow (`--lift-2`), as `theme::CARD_SHADOW`.
        shadow_color: Pair::new(&alpha(w.ink, 8.0), &alpha(Rgba8::BLACK, 60.0)),
        font: format!("'{FONT_FAMILY}', system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif"),
        radius_sm: f32::from(theme::RADIUS_SM),
        radius: f32::from(theme::RADIUS),
        radius_lg: f32::from(theme::RADIUS_LG),
        ..Tokens::garage44()
    }
}

/// [`tokens`] on the app's type scale, for the library's screens in the
/// app (`mithril_bevy`, and the web client's frame): egui's sizes
/// (`theme::SIZE_BODY` 14, `SIZE_SUB` 12.5, `SIZE_TITLE` 17) rather than
/// the site's 15 / 13 / 17, so a screen moved off egui keeps its density.
/// On the site's scale every library screen read a size larger than the
/// egui one beside it, and titles wrapped that did not before. The site's
/// `theme.css` stays on [`tokens`].
#[must_use]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub fn app_tokens() -> Tokens {
    Tokens {
        font_size: theme::SIZE_BODY,
        font_size_sm: theme::SIZE_SUB,
        font_size_lg: theme::SIZE_TITLE,
        // An option's icon (`Opt::icon`) in the app's icon font, as every
        // icon in the app's screens is (`icons::FONT`).
        font_icons: "'Material Symbols Rounded'".to_owned(),
        // The app's accent plates, not the web's (`WEB`): the deeper teal
        // with white ink, as egui's are since 2026-11-10 ("a teal button
        // with black text is not nice").
        accent: pair(theme::LIGHT.accent, theme::DARK.accent),
        accent_hover: pair(theme::LIGHT.accent_lift, theme::DARK.accent_lift),
        on_accent: pair(theme::LIGHT.on_accent, theme::DARK.on_accent),
        focus: Pair::new(
            &alpha(theme::LIGHT.accent, 50.0),
            &alpha(theme::DARK.accent, 55.0),
        ),
        ..tokens()
    }
}

/// The map chrome's own palette (`theme::map_disc`, the dock, the search
/// bar) as custom properties for a library root over the map: `--map-fill`,
/// `--map-rim`, `--map-ink`. The panels' surface and border are a shade
/// off it, and its ink is white where theirs is the accent.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub fn map_vars() -> String {
    let p = theme::p();
    format!(
        "--map-fill: {}; --map-rim: {}; --map-ink: {}",
        hex(p.map_fill),
        hex(p.map_rim),
        hex(p.map_ink)
    )
}
