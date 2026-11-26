//! Every colour the theme names, pinned in `golden_colours.txt`.
//!
//! Written before the theme moved off `egui::Color32` onto its own
//! [`Rgba8`](super::Rgba8) (egui-removal stage S3, 2026-11-26), so the move
//! could be shown to change no visible colour. With egui gone each line is a
//! value's straight (unmultiplied) bytes. A table that still matches after an
//! edit to `palette.rs` or `tokens.rs` changed nothing on screen; one that
//! does not names the value that moved.
//!
//! ```text
//! CARTO_WRITE_THEME_GOLDEN=1 cargo test -p cartopolis theme::golden
//! ```
//!
//! rewrites the file, for a change of colour that is meant.

use std::fmt::Write as _;

use super::tokens::{self, WEB};
use super::{brand, scenario, Palette, Rgba8, DARK, LIGHT};

const FILE: &str = "src/systems/gui/theme/golden_colours.txt";

fn line(out: &mut String, name: &str, c: Rgba8) {
    let [r, g, b, a] = c.to_array();
    writeln!(out, "{name:<32} straight {r:02x}{g:02x}{b:02x}{a:02x}").unwrap();
}

fn palette(out: &mut String, name: &str, p: &Palette) {
    macro_rules! fields {
        ($($f:ident),* $(,)?) => {
            $( line(out, &format!("{name}.{}", stringify!($f)), p.$f); )*
        };
    }
    fields!(
        ink,
        on_accent,
        on_status,
        surface,
        surface_raised,
        surface_hover,
        line,
        accent,
        accent_bright,
        accent_lift,
        accent_ring,
        accent_pressed,
        accent_text,
        accent_word,
        accent_muted,
        cream,
        text,
        text_dim,
        warn,
        error,
        overlay,
        glass,
        panel,
        card,
        panel_rim,
        overlay_line,
        overlay_soft,
        list_line,
        tile_on,
        knob_on,
        gold,
        on_gold,
        control_edge,
        map_fill,
        map_ink,
        map_rim,
        map_mark,
    );
    writeln!(out, "{name}.shadow {}  dark {}", p.shadow, p.dark).unwrap();
}

fn tokens_of(out: &mut String, name: &str, t: &mithril_style::Tokens) {
    macro_rules! pairs {
        ($($f:ident),* $(,)?) => {
            $( writeln!(out, "{name}.{:<20} {} | {}", stringify!($f), t.$f.light.0, t.$f.dark.0).unwrap(); )*
        };
    }
    pairs!(
        accent,
        accent_hover,
        accent_text,
        accent_soft,
        on_accent,
        bg,
        surface,
        surface_raised,
        surface_sunken,
        hover,
        pressed,
        text,
        text_strong,
        text_dim,
        text_faint,
        border,
        border_strong,
        control_bg,
        info,
        info_soft,
        success,
        success_soft,
        warning,
        warning_soft,
        danger,
        danger_soft,
        focus,
        backdrop,
        shadow_color,
    );
}

/// The whole table, in a fixed order.
fn table() -> String {
    let mut out = String::new();
    palette(&mut out, "DARK", &DARK);
    palette(&mut out, "LIGHT", &LIGHT);
    palette(&mut out, "WEB", &WEB);

    macro_rules! named {
        ($($path:path),* $(,)?) => {
            $( line(&mut out, stringify!($path), $path); )*
        };
    }
    named!(
        super::BRAND,
        super::REAL_LOCATION,
        super::PUBLIC,
        super::COUNCIL,
        super::COUNCIL_LIGHT,
        super::LABEL_CHIP,
        super::LABEL_HALO,
        super::LABEL_INK,
        super::CALL_END_RED,
        super::CALL_ACCEPT_GREEN,
        super::WORDMARK_TAIL_COLOR,
        brand::mark::PLATE,
        brand::mark::STREET,
        brand::mark::WATER,
        scenario::LIVE,
        scenario::REPLAY,
        scenario::EXERCISE,
        scenario::SANDBOX,
        scenario::TRACK,
        scenario::TRACK_GLOW,
        scenario::CONE,
        scenario::CONE_EDGE,
        scenario::AREA_EDGE,
        scenario::AREA_HALO,
        scenario::HEAD,
        scenario::HEAD_DOT,
        scenario::HEAD_RIM,
        scenario::INTERCEPT,
        tokens::L_BG,
        tokens::L_SURFACE,
        tokens::L_SURFACE_RAISED,
        tokens::L_SURFACE_SUNKEN,
        tokens::L_TEXT,
        tokens::L_TEXT_DIM,
        tokens::L_TEXT_FAINT,
        tokens::L_BORDER,
        tokens::L_BORDER_STRONG,
        tokens::L_ACCENT_HOVER,
        tokens::L_ACCENT_TEXT,
        tokens::L_ACCENT_SOFT,
        tokens::L_INFO,
        tokens::D_INFO,
        tokens::L_INFO_SOFT,
        tokens::D_INFO_SOFT,
        tokens::L_SUCCESS,
        tokens::D_SUCCESS,
        tokens::L_SUCCESS_SOFT,
        tokens::D_SUCCESS_SOFT,
        tokens::L_WARNING,
        tokens::D_WARNING,
        tokens::L_WARNING_SOFT,
        tokens::D_WARNING_SOFT,
        tokens::L_DANGER,
        tokens::D_DANGER,
        tokens::L_DANGER_SOFT,
        tokens::D_DANGER_SOFT,
    );

    // `p()` answers each theme in turn; the light switch is process-global,
    // so this reads the tables directly rather than flipping it.
    let gold_dark = DARK.warn;
    let gold_light = LIGHT.warn;
    line(&mut out, "midas_gold (dark)", gold_dark);
    line(&mut out, "midas_gold (light)", gold_light);

    tokens_of(&mut out, "tokens", &tokens::tokens());
    tokens_of(&mut out, "app_tokens", &tokens::app_tokens());
    out
}

#[test]
fn every_theme_colour_is_the_one_pinned() {
    let have = table();
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FILE);
    if std::env::var_os("CARTO_WRITE_THEME_GOLDEN").is_some() {
        std::fs::write(&file, &have).unwrap();
    }
    let want = std::fs::read_to_string(&file).unwrap_or_default();
    if have != want {
        let diff: Vec<_> = have
            .lines()
            .zip(want.lines().chain(std::iter::repeat("<missing>")))
            .filter(|(h, w)| h != w)
            .map(|(h, w)| format!("  have {h}\n  want {w}"))
            .collect();
        panic!(
            "{} theme colour(s) moved ({} lines vs {}):\n{}",
            diff.len(),
            have.lines().count(),
            want.lines().count(),
            diff.join("\n")
        );
    }
}
