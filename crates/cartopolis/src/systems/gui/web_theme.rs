//! The web UI's theme, written from this crate's: `crates/atlas/web/src/css/theme.css`.
//!
//! cartopolis.org is a Mithril app in another language, styled by the fork's
//! component library through its `--m-*` custom properties. It used to take its
//! palette from [`theme`] by hand, and the copy stopped being one: the client
//! went from slate blue to charcoal and orange (2026-09/10) while the web page
//! stayed slate blue, so the two halves of the product looked like two products.
//! So the web's tokens are now **generated** from the constants the client draws
//! with, and a test fails when the checked-in file is not what this module
//! would write.
//!
//! ```text
//! CARTO_WRITE_WEB_THEME=1 cargo test -p cartopolis web_theme
//! ```
//!
//! **The dark half is the client.** Every dark value below is a [`theme`]
//! constant; none is a fresh hex. **The light half is the web's own** — the
//! client has no light mode, and a phone in daylight on a form needs one — and
//! lives here so it is still in the one place UI colour is chosen, beside the
//! dark value it answers to. It is the same ramp turned over, with the same
//! teal cast, and the same [`theme::ACCENT`] carrying
//! [`theme::INK`] in both modes, so a primary button is the same object in the
//! client, on the dark page and on the light one.
//!
//! What is *not* generated is the page styling itself (`app.css` and the page
//! stylesheets), which only ever names `--m-*` tokens — that rule is what makes
//! this file the whole palette.
//!
//! Test-only on purpose: nothing at run time reads it, and a module compiled
//! into the client binary to be called from one test is dead weight there.

use super::theme;
use egui::Color32;

/// Where the generated file lives, from this crate's manifest directory.
const OUT: &str = "../atlas/web/src/css/theme.css";
/// And the page whose pre-stylesheet background must match `--m-bg`.
const INDEX_HTML: &str = "../atlas/web/src/index.html";

/// The font family name the web's `@font-face` rules declare (`app.css`). The
/// files are the client's own, copied into `dist/fonts/` by `build.ts`.
const FONT_FAMILY: &str = "Plus Jakarta Sans";

// ─── The light half ───────────────────────────────────────────────────────────

/// Page background: an off-white with the ramp's teal cast, the light twin of
/// [`theme::INK`].
const L_BG: Color32 = Color32::from_rgb(0xF1, 0xF5, 0xF5);
const L_SURFACE: Color32 = Color32::WHITE;
const L_SURFACE_RAISED: Color32 = Color32::from_rgb(0xF8, 0xFA, 0xFA);
const L_SURFACE_SUNKEN: Color32 = Color32::from_rgb(0xE7, 0xED, 0xED);
/// Body text is the client's raised surface: the ramp, read the other way.
const L_TEXT: Color32 = theme::SURFACE_RAISED;
const L_TEXT_DIM: Color32 = Color32::from_rgb(0x54, 0x65, 0x68);
const L_TEXT_FAINT: Color32 = Color32::from_rgb(0x84, 0x97, 0x9A);
const L_BORDER: Color32 = Color32::from_rgb(0xDA, 0xE3, 0xE3);
/// A control's boundary. Clears 3:1 on white for the reason [`theme::LINE`]
/// does on the dark ramp (WCAG 1.4.11): a field at rest has no other edge.
const L_BORDER_STRONG: Color32 = Color32::from_rgb(0x84, 0x97, 0x9A);
/// Pushed under the pointer: the same move as [`theme::ACCENT_PRESSED`].
const L_ACCENT_HOVER: Color32 = theme::ACCENT_PRESSED;
/// Links and accent-coloured words on white. The teal itself is too light to
/// read there, so the text tint is a deep version of it.
const L_ACCENT_TEXT: Color32 = Color32::from_rgb(0x17, 0x69, 0x6A);
/// Selected rows, a pressed nav item: the light twin of [`theme::ACCENT_MUTED`].
const L_ACCENT_SOFT: Color32 = Color32::from_rgb(0xD9, 0xF0, 0xEE);

// ─── Status tones (the client has two: WARN and ERROR) ───────────────────────

const L_INFO: Color32 = Color32::from_rgb(0x1F, 0x6A, 0xB0);
const D_INFO: Color32 = theme::REAL_LOCATION;
const L_INFO_SOFT: Color32 = Color32::from_rgb(0xE4, 0xF0, 0xFB);
const D_INFO_SOFT: Color32 = Color32::from_rgb(0x15, 0x27, 0x36);
const L_SUCCESS: Color32 = Color32::from_rgb(0x1C, 0x74, 0x42);
const D_SUCCESS: Color32 = Color32::from_rgb(0x7C, 0xCF, 0x9B);
const L_SUCCESS_SOFT: Color32 = Color32::from_rgb(0xE3, 0xF4, 0xE9);
const D_SUCCESS_SOFT: Color32 = Color32::from_rgb(0x15, 0x2C, 0x1E);
const L_WARNING: Color32 = Color32::from_rgb(0x85, 0x57, 0x00);
const D_WARNING: Color32 = theme::WARN;
const L_WARNING_SOFT: Color32 = Color32::from_rgb(0xFB, 0xF1, 0xDA);
const D_WARNING_SOFT: Color32 = Color32::from_rgb(0x30, 0x29, 0x16);
const L_DANGER: Color32 = Color32::from_rgb(0xB3, 0x36, 0x1E);
const D_DANGER: Color32 = theme::ERROR;
const L_DANGER_SOFT: Color32 = Color32::from_rgb(0xFC, 0xE8, 0xE4);
const D_DANGER_SOFT: Color32 = Color32::from_rgb(0x36, 0x1D, 0x19);

/// One token: a light/dark pair, or one value for both.
enum Value {
    Pair(String, String),
    Both(String),
}

/// An opaque colour as CSS.
fn hex(c: Color32) -> String {
    debug_assert_eq!(c.a(), 255, "a translucent colour goes through alpha()");
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// An opaque colour at a percentage, as CSS. Built from the opaque colour
/// rather than from a translucent `Color32`, which egui stores premultiplied —
/// reading one back at 5 % alpha does not return the colour it was made from.
fn alpha(c: Color32, pct: f32) -> String {
    format!("rgb({} {} {} / {}%)", c.r(), c.g(), c.b(), pct.round())
}

fn pair(light: Color32, dark: Color32) -> Value {
    Value::Pair(hex(light), hex(dark))
}

fn both(c: Color32) -> Value {
    Value::Both(hex(c))
}

/// Every token, in the order the file lists them. Names are the library's
/// (`@garage44/mithril/components`, `css/tokens.css`) plus `--c-*` for the two
/// this project has and the library does not.
fn tokens() -> Vec<(&'static str, Value)> {
    use theme::*;
    use Value::*;
    // [`GLASS`]'s own alpha, so the web's glass is the client's.
    let glass_pct = GLASS.a() as f32 / 2.55;
    vec![
        // Brand
        ("--c-brand", both(BRAND)),
        ("--m-accent", both(ACCENT)),
        ("--m-accent-hover", pair(L_ACCENT_HOVER, ACCENT_RING)),
        // Dark: the pale teal, so words in it read as well on the selected
        // fill ([`ACCENT_MUTED`]) as on a panel.
        ("--m-accent-text", pair(L_ACCENT_TEXT, ACCENT_RING)),
        ("--m-accent-soft", pair(L_ACCENT_SOFT, ACCENT_MUTED)),
        ("--m-on-accent", both(INK)),
        // Surfaces, back to front
        ("--m-bg", pair(L_BG, INK)),
        ("--m-surface", pair(L_SURFACE, SURFACE)),
        ("--m-surface-raised", pair(L_SURFACE_RAISED, SURFACE_RAISED)),
        ("--m-surface-sunken", pair(L_SURFACE_SUNKEN, PANEL)),
        (
            "--c-glass",
            Pair(alpha(L_SURFACE, glass_pct), alpha(SURFACE, glass_pct)),
        ),
        ("--m-hover", Pair(alpha(INK, 5.0), alpha(CREAM, 6.0))),
        ("--m-pressed", Pair(alpha(INK, 10.0), alpha(CREAM, 11.0))),
        // Text and lines
        ("--m-text", pair(L_TEXT, TEXT)),
        ("--m-text-strong", pair(INK, CREAM)),
        ("--m-text-dim", pair(L_TEXT_DIM, TEXT_DIM)),
        ("--m-text-faint", pair(L_TEXT_FAINT, LINE)),
        ("--m-border", pair(L_BORDER, SURFACE_HOVER)),
        ("--m-border-strong", pair(L_BORDER_STRONG, LINE)),
        ("--m-control-bg", pair(L_SURFACE, INK)),
        // Status
        ("--m-info", pair(L_INFO, D_INFO)),
        ("--m-info-soft", pair(L_INFO_SOFT, D_INFO_SOFT)),
        ("--m-success", pair(L_SUCCESS, D_SUCCESS)),
        ("--m-success-soft", pair(L_SUCCESS_SOFT, D_SUCCESS_SOFT)),
        ("--m-warning", pair(L_WARNING, D_WARNING)),
        ("--m-warning-soft", pair(L_WARNING_SOFT, D_WARNING_SOFT)),
        ("--m-danger", pair(L_DANGER, D_DANGER)),
        ("--m-danger-soft", pair(L_DANGER_SOFT, D_DANGER_SOFT)),
        ("--m-focus", Pair(alpha(ACCENT, 50.0), alpha(ACCENT, 55.0))),
        (
            "--m-backdrop",
            Pair(alpha(INK, 40.0), alpha(Color32::BLACK, 60.0)),
        ),
        (
            "--m-shadow-color",
            Pair(alpha(INK, 8.0), alpha(Color32::BLACK, 45.0)),
        ),
        // Shape and type
        (
            "--m-font",
            Both(format!(
                "'{FONT_FAMILY}', system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif"
            )),
        ),
        ("--m-radius-sm", Both(format!("{RADIUS_SM}px"))),
        ("--m-radius", Both(format!("{RADIUS}px"))),
        ("--m-radius-lg", Both(format!("{RADIUS_LG}px"))),
    ]
}

/// The whole of `theme.css`.
pub fn css() -> String {
    let mut out = String::from(
        "/*\n \
         * GENERATED from crates/cartopolis/src/systems/gui/theme.rs by\n \
         * crates/cartopolis/src/systems/gui/web_theme.rs — do not edit. Change the\n \
         * constant there and run\n \
         *\n \
         *     CARTO_WRITE_WEB_THEME=1 cargo test -p cartopolis web_theme\n \
         *\n \
         * The dark half is the client's palette, value for value; the light half\n \
         * is the web's own, chosen beside it. Imported after the component\n \
         * library's tokens, which these override (same selector, later wins).\n \
         */\n\n\
         :root,\n[data-theme] {\n",
    );
    for (name, value) in tokens() {
        let v = match value {
            Value::Pair(light, dark) => format!("light-dark({light}, {dark})"),
            Value::Both(v) => v,
        };
        out.push_str(&format!("    {name}: {v};\n"));
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use theme::*;

    fn path(rel: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

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

    /// The checked-in file is what this module writes. On a mismatch, either
    /// somebody edited the generated file (edit the constant instead) or a
    /// constant moved and the web was not regenerated.
    #[test]
    fn web_theme_css_is_generated_from_the_client_palette() {
        let want = css();
        let file = path(OUT);
        if std::env::var_os("CARTO_WRITE_WEB_THEME").is_some() {
            std::fs::write(&file, &want).unwrap();
        }
        let have = std::fs::read_to_string(&file).unwrap_or_default();
        assert!(
            have == want,
            "{} is stale: run `CARTO_WRITE_WEB_THEME=1 cargo test -p cartopolis web_theme`",
            file.display()
        );
    }

    /// `index.html` paints the page background before the stylesheet arrives,
    /// so a phone never flashes the wrong one. It is the one place outside the
    /// generated file that names a colour, and it must name `--m-bg`'s.
    #[test]
    fn web_index_paints_the_generated_background() {
        let html = std::fs::read_to_string(path(INDEX_HTML)).unwrap();
        for (mode, c) in [("light", L_BG), ("dark", INK)] {
            let want = hex(c);
            assert!(
                html.contains(&format!("content=\"{want}\""))
                    && html.contains(&format!("background: {want}")),
                "index.html's {mode} theme-color and background must be {want} (--m-bg)"
            );
        }
    }

    /// The light half has no client to have been measured in, so it is
    /// measured here — the same pairs the library's own note lists.
    #[test]
    fn web_text_tones_clear_their_surfaces() {
        let pairs = [
            ("light body", L_TEXT, L_SURFACE, 4.5),
            ("light body on bg", L_TEXT, L_BG, 4.5),
            ("light dim", L_TEXT_DIM, L_SURFACE, 4.5),
            ("light dim on bg", L_TEXT_DIM, L_BG, 4.5),
            ("light dim on sunken", L_TEXT_DIM, L_SURFACE_SUNKEN, 4.5),
            ("light link", L_ACCENT_TEXT, L_SURFACE, 4.5),
            ("light link on soft", L_ACCENT_TEXT, L_ACCENT_SOFT, 4.5),
            ("light ink on accent", INK, ACCENT, 4.5),
            ("light ink on hover", INK, L_ACCENT_HOVER, 4.5),
            ("light control edge", L_BORDER_STRONG, L_SURFACE, 3.0),
            ("light info", L_INFO, L_INFO_SOFT, 4.5),
            ("light success", L_SUCCESS, L_SUCCESS_SOFT, 4.5),
            ("light warning", L_WARNING, L_WARNING_SOFT, 4.5),
            ("light danger", L_DANGER, L_DANGER_SOFT, 4.5),
            ("dark body on bg", TEXT, INK, 4.5),
            ("dark dim on raised", TEXT_DIM, SURFACE_RAISED, 4.5),
            ("dark link", ACCENT_RING, SURFACE, 4.5),
            ("dark link on soft", ACCENT_RING, ACCENT_MUTED, 4.5),
            ("dark ink on hover", INK, ACCENT_RING, 4.5),
            ("dark control edge", LINE, SURFACE, 3.0),
            ("dark info", D_INFO, D_INFO_SOFT, 4.5),
            ("dark success", D_SUCCESS, D_SUCCESS_SOFT, 4.5),
            ("dark warning", D_WARNING, D_WARNING_SOFT, 4.5),
            ("dark danger", D_DANGER, D_DANGER_SOFT, 4.5),
        ];
        let failing: Vec<String> = pairs
            .iter()
            .filter_map(|&(name, fg, bg, min)| {
                let c = contrast(fg, bg);
                (c < min).then(|| format!("{name}: {c:.2} < {min}"))
            })
            .collect();
        assert!(failing.is_empty(), "{failing:#?}");
    }
}
