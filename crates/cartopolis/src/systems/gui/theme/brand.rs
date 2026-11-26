use super::*;

// ─── The mark ─────────────────────────────────────────────────────────────────

/// The app mark, **drawn rather than sampled**: the brand pin standing on
/// Groningen's street plan, the streets cleared in a band around it so the pin
/// reads as cut out of the map.
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
/// **The street plan is data, not a hand-written table**: `mark_table.rs` is
/// written by `tools/brand-mark.ts` from OpenStreetMap in the same run that
/// writes `assets/icon/cartopolis.svg`, its light variant, the PNG, the .ico and
/// `index.html`'s loader copy — the web splash and this one are one screen
/// handed over mid-load (see `systems::splash`), and a mark that changes at the
/// handover is the tell that they are not. Rerun the tool; do not edit any of
/// them by hand. The one painting simplification against the icon is the tile's
/// vertical gradient, which is its middle tone here (egui has no gradient
/// primitive).
pub(super) mod mark {
    use super::Rgba8;

    /// Half the icon's square. Every coordinate in the table is in that square's
    /// units with `(UNIT, UNIT)` at the centre, so `paint_mark` maps them with
    /// one scale factor of `radius / UNIT`.
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

/// Draw [the mark](mark) centred on `center`, out to `radius` (half the tile, so
/// the splash's 84 gives a 168 pt square), at `alpha` — 1.0 opaque; the splash
/// fades the whole screen out through it.
pub fn paint_mark(p: &egui::Painter, center: egui::Pos2, radius: f32, alpha: f32) {
    // `gamma_multiply` rather than a rebuilt `from_rgba_unmultiplied`: `Color32`
    // is premultiplied, so re-wrapping its channels as straight ones darkens
    // anything that already carries an alpha.
    let a = |c: Rgba8| Color32::from(c).gamma_multiply(alpha);
    let s = radius / mark::UNIT;
    // Icon units → screen.
    let q = |v: [f32; 2]| {
        egui::pos2(
            center.x + (v[0] - mark::UNIT) * s,
            center.y + (v[1] - mark::UNIT) * s,
        )
    };
    let detail = radius >= mark::DETAIL_RADIUS;

    p.rect_filled(
        egui::Rect::from_center_size(center, egui::Vec2::splat(radius * 2.0)),
        CornerRadius::same((mark::PLATE_CORNER * s).round().clamp(0.0, 255.0) as u8),
        a(mark::PLATE),
    );

    // The plan is already cut to the tile and clear of the pin, so it is drawn
    // with no clip — egui has none to give it.
    for line in mark::LINES {
        if !detail && line.width < mark::MAJOR_WIDTH {
            continue;
        }
        let ink = match line.ink {
            mark::Ink::Street => mark::STREET,
            mark::Ink::Water => mark::WATER,
        };
        p.add(egui::Shape::line(
            line.points.iter().map(|&v| q(v)).collect(),
            Stroke::new((line.width * s).max(0.6), a(ink)),
        ));
    }

    // The teardrop is convex, so it is one polygon; its keyline is the polygon's
    // own stroke, centred on the edge as the SVG's is.
    p.add(egui::Shape::convex_polygon(
        mark::PIN_OUTLINE.iter().map(|&v| q(v)).collect(),
        a(BRAND),
        Stroke::new((mark::PIN_KEYLINE * s).max(1.0), a(MARK_CREAM)),
    ));
    p.circle_filled(q(mark::PIN_DOT_C), mark::PIN_DOT_R * s, a(MARK_CREAM));
}

/// The mark's cream: the pin's keyline and dot, `#F2EFEA` in every copy of the
/// icon. Not [`Palette::cream`], which is the chrome's text colour and a cooler white.
pub(super) const MARK_CREAM: Rgba8 = Rgba8::from_rgb(0xF2, 0xEF, 0xEA);

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

/// Paint the wordmark centred on `center` at `size` points, faded by `alpha`.
///
/// Laid out as two galleys and placed from their combined width, because
/// `Align2::CENTER_CENTER` on each piece separately would centre each of them on
/// the same point and stack the two words on top of each other.
///
/// Cream on the splash's dark in both themes ([`DARK`]): the splash is the
/// same screen as `index.html`'s loader, which is dark.
pub fn paint_wordmark(p: &egui::Painter, center: egui::Pos2, size: f32, alpha: f32) {
    let a = |c: Rgba8| {
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * alpha) as u8)
    };
    let font = FontId::new(size, brand_family());
    let head = p.layout_no_wrap(WORDMARK_HEAD.to_owned(), font.clone(), a(DARK.cream));
    let tail = p.layout_no_wrap(WORDMARK_TAIL.to_owned(), font, a(WORDMARK_TAIL_COLOR));
    let total = head.size().x + tail.size().x;
    let left = center.x - total * 0.5;
    let top = center.y - head.size().y * 0.5;
    let head_w = head.size().x;
    p.galley(egui::pos2(left, top), head, a(DARK.cream));
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
                .color(p32().cream),
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
