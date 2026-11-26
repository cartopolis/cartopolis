use super::*;

// ─── The mark ─────────────────────────────────────────────────────────────────

/// The app mark as a table: the brand pin standing on Groningen's street plan,
/// the streets cleared in a band around it so the pin reads as cut out of the
/// map.
///
/// **Nothing draws it any more.** egui painted it on the boot splash and the
/// sign-in card; the library's editions show `assets/icon/cartopolis.svg`
/// instead (`lib_splash`, `lib_login`), which is the same mark. The table
/// stays because `tools/brand-mark.ts` still writes it in the run that writes
/// that SVG, its light variant, the PNG, the .ico and `index.html`'s loader
/// copy, and `tests::the_pin_outline_is_convex` and
/// `the_plan_stays_inside_the_tile_and_clear_of_the_pin` still check the
/// tool's output through it. Drop it together with the tool's Rust output.
/// Rerun the tool; do not edit it by hand.
#[allow(dead_code)]
pub(super) mod mark {
    use super::Rgba8;

    /// Half the icon's square. Every coordinate in the table is in that square's
    /// units with `(UNIT, UNIT)` at the centre.
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
    pub const PLATE: Rgba8 = Rgba8::from_rgb(0x25, 0x35, 0x3F);
    /// The street plan's two inks — cream at 50 % and 30 % over the plate,
    /// pre-mixed so crossing strokes do not double up.
    pub const STREET: Rgba8 = Rgba8::from_rgb(0x8C, 0x92, 0x95);
    pub const WATER: Rgba8 = Rgba8::from_rgb(0x62, 0x6D, 0x72);

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
pub const WORDMARK_TAIL_COLOR: Rgba8 = BRAND;

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
