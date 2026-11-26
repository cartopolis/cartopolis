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
//! The `--m-*` tokens are [`theme::tokens`], the one `Tokens` value the Bevy
//! backend draws with too (phase 4 of `docs/mockups/component-library`); this
//! module adds the `--c-*` tokens the page has and the library does not, and
//! writes the file. The colours themselves, light and dark, are chosen in
//! `theme/tokens.rs`.
//!
//! What is *not* generated is the page styling itself (`app.css` and the page
//! stylesheets), which only ever names `--m-*` tokens — that rule is what makes
//! this file the whole palette.
//!
//! Test-only on purpose: nothing at run time reads it, and a module compiled
//! into the client binary to be called from one test is dead weight there.

use mithril_style::Pair;

use super::theme;
use super::theme::tokens::{alpha, hex, WEB};
use super::theme::Rgba8;

/// Where the generated file lives, from this crate's manifest directory.
const OUT: &str = "../atlas/web/src/css/theme.css";
/// And the page whose pre-stylesheet background must match `--m-bg`.
const INDEX_HTML: &str = "../atlas/web/src/index.html";

/// One token: a light/dark pair, or one value for both.
enum Value {
    Pair(String, String),
    Both(String),
}

impl From<&Pair> for Value {
    fn from(p: &Pair) -> Self {
        if p.light == p.dark {
            Value::Both(p.light.0.clone())
        } else {
            Value::Pair(p.light.0.clone(), p.dark.0.clone())
        }
    }
}

fn both(c: Rgba8) -> Value {
    Value::Both(hex(c))
}

/// Every token, in the order the file lists them. The `--m-*` names are the
/// library's (`@garage44/mithril/components`, `css/tokens.css`), their values
/// [`theme::tokens`]'s; `--c-*` are the ones this project has and the library
/// does not.
fn tokens() -> Vec<(&'static str, Value)> {
    use theme::*;
    use Value::*;
    let t = theme::tokens();
    // The web's glass stays at the 92 % the client's was. The client went
    // solid on 2026-11-08 (`theme::GLASS`) at the person's word about the app;
    // the web page was not part of that ask, so it keeps its look until it is.
    let glass_pct = 92.0;
    let l_surface = Rgba8::WHITE;
    vec![
        // Brand
        ("--c-brand", both(BRAND)),
        ("--m-accent", (&t.accent).into()),
        ("--m-accent-hover", (&t.accent_hover).into()),
        ("--m-accent-text", (&t.accent_text).into()),
        ("--m-accent-soft", (&t.accent_soft).into()),
        ("--m-on-accent", (&t.on_accent).into()),
        // Surfaces, back to front
        ("--m-bg", (&t.bg).into()),
        ("--m-surface", (&t.surface).into()),
        ("--m-surface-raised", (&t.surface_raised).into()),
        ("--m-surface-sunken", (&t.surface_sunken).into()),
        (
            "--c-glass",
            Pair(alpha(l_surface, glass_pct), alpha(WEB.surface, glass_pct)),
        ),
        ("--m-hover", (&t.hover).into()),
        ("--m-pressed", (&t.pressed).into()),
        // Text and lines
        ("--m-text", (&t.text).into()),
        ("--m-text-strong", (&t.text_strong).into()),
        ("--m-text-dim", (&t.text_dim).into()),
        ("--m-text-faint", (&t.text_faint).into()),
        ("--m-border", (&t.border).into()),
        ("--m-border-strong", (&t.border_strong).into()),
        ("--m-control-bg", (&t.control_bg).into()),
        // Status
        ("--m-info", (&t.info).into()),
        ("--m-info-soft", (&t.info_soft).into()),
        ("--m-success", (&t.success).into()),
        ("--m-success-soft", (&t.success_soft).into()),
        ("--m-warning", (&t.warning).into()),
        ("--m-warning-soft", (&t.warning_soft).into()),
        ("--m-danger", (&t.danger).into()),
        ("--m-danger-soft", (&t.danger_soft).into()),
        ("--m-focus", (&t.focus).into()),
        ("--m-backdrop", (&t.backdrop).into()),
        ("--m-shadow-color", (&t.shadow_color).into()),
        // Depth: what makes a surface read as a pane of glass and a button as
        // a thing standing on it (`depth.css`). Light from above: a rim and an
        // inner highlight along the top edge, a shade along the bottom, and the
        // accent's own glow under whatever is live.
        // Dark: the client's glass, so a card is the midas hub's card — solid
        // [`SURFACE`] on [`INK`] to the eye, with the street map behind only a
        // whisper. It was the raised step at 55 %, a lighter, thinner pane than
        // anything the app or the hub draws (2026-11-01).
        (
            "--c-glass-soft",
            Pair(alpha(l_surface, 62.0), alpha(WEB.surface, glass_pct)),
        ),
        (
            "--c-rim",
            Pair(alpha(Rgba8::WHITE, 85.0), alpha(WEB.cream, 9.0)),
        ),
        (
            "--c-rim-strong",
            Pair(alpha(Rgba8::WHITE, 100.0), alpha(WEB.cream, 16.0)),
        ),
        (
            "--c-highlight",
            Pair(alpha(Rgba8::WHITE, 90.0), alpha(WEB.cream, 8.0)),
        ),
        (
            "--c-shade",
            Pair(alpha(WEB.ink, 7.0), alpha(Rgba8::BLACK, 35.0)),
        ),
        (
            "--c-glow",
            Pair(alpha(WEB.accent_pressed, 35.0), alpha(WEB.accent, 32.0)),
        ),
        ("--c-accent-top", both(WEB.accent_ring)),
        ("--c-accent-bottom", both(WEB.accent_pressed)),
        // The page behind the glass: Groningen's streets in one faint ink, so
        // a translucent pane has something with structure to blur. Glows and
        // a grid were tried first (2026-10-10) and were too even to read as
        // anything behind the glass.
        (
            "--c-map",
            Pair(alpha(WEB.ink, 7.0), alpha(WEB.text_dim, 9.0)),
        ),
        // Shape and type
        ("--m-font", Both(t.font.clone())),
        (
            "--m-radius-sm",
            Both(format!("{}px", mithril_style::num(t.radius_sm))),
        ),
        (
            "--m-radius",
            Both(format!("{}px", mithril_style::num(t.radius))),
        ),
        (
            "--m-radius-lg",
            Both(format!("{}px", mithril_style::num(t.radius_lg))),
        ),
    ]
}

/// The whole of `theme.css`.
pub fn css() -> String {
    let mut out = String::from(
        "/*\n \
         * GENERATED from crates/cartopolis/src/systems/gui/theme/ by\n \
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
    use super::theme::tokens::*;
    use super::*;

    fn path(rel: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    fn luminance(c: Rgba8) -> f64 {
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

    fn contrast(a: Rgba8, b: Rgba8) -> f64 {
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
        for (mode, c) in [("light", L_BG), ("dark", WEB.ink)] {
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
            ("light ink on accent", WEB.ink, WEB.accent, 4.5),
            ("light ink on hover", WEB.ink, L_ACCENT_HOVER, 4.5),
            ("light control edge", L_BORDER_STRONG, L_SURFACE, 3.0),
            ("light info", L_INFO, L_INFO_SOFT, 4.5),
            ("light success", L_SUCCESS, L_SUCCESS_SOFT, 4.5),
            ("light warning", L_WARNING, L_WARNING_SOFT, 4.5),
            ("light danger", L_DANGER, L_DANGER_SOFT, 4.5),
            ("dark body on bg", WEB.text, WEB.ink, 4.5),
            ("dark dim on raised", WEB.text_dim, WEB.surface_raised, 4.5),
            ("dark link", WEB.accent_ring, WEB.surface, 4.5),
            ("dark link on soft", WEB.accent_ring, WEB.accent_muted, 4.5),
            ("dark ink on hover", WEB.ink, WEB.accent_ring, 4.5),
            ("dark control edge", WEB.line, WEB.surface, 3.0),
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
