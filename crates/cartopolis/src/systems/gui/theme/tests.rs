use super::*;
use crate::systems::egui_manager::UnpaintedPasses as _;

/// Relative luminance per WCAG 2.x, on a colour's sRGB bytes. Every colour
/// these tests measure is opaque (translucent ones go through [`over`]
/// first), so taking a [`Color32`] — what egui's visuals hand back — or an
/// [`Rgba8`] reads the same bytes.
fn luminance(c: impl Into<Color32>) -> f64 {
    let c: Color32 = c.into();
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

fn contrast(a: impl Into<Color32>, b: impl Into<Color32>) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// Both palettes, named, for the tests that hold in each.
const BOTH: [(&str, &Palette); 2] = [("dark", &DARK), ("light", &LIGHT)];

/// The palette's whole claim is that it is *legible*, and a colour is very
/// easy to nudge into looking nicer and reading worse. Pin the pairs that
/// actually occur: text on a panel, text on a selected row, the ink on the
/// accent plate — in both themes.
#[test]
fn text_colours_clear_their_backgrounds() {
    for (theme, c) in BOTH {
        for (name, fg, bg, min) in [
            ("body on panel", c.text, c.surface, 4.5),
            ("body on raised", c.text, c.surface_raised, 4.5),
            ("dim on panel", c.text_dim, c.surface, 4.5),
            ("heading on panel", c.cream, c.surface, 4.5),
            ("link on panel", c.accent_text, c.surface, 4.5),
            ("selected row", c.cream, c.accent_muted, 4.5),
            ("label on pressed button", c.cream, c.accent_muted, 4.5),
            ("accent word on its tag", c.accent_word, c.accent_muted, 4.5),
            ("ink on an engaged plate", c.on_accent, c.accent, 4.5),
            // The three fills [`primary_button_sized`] moves between, all
            // labelled in the plate's ink.
            ("primary label, hovered", c.on_accent, c.accent_lift, 4.5),
            ("primary label, pressed", c.on_accent, c.accent_pressed, 4.5),
            ("warning on panel", c.warn, c.surface, 4.5),
            ("error on panel", c.error, c.surface, 4.5),
            ("ink on an armed confirm", c.on_status, c.error, 4.5),
            ("ink on a warning plate", c.on_status, c.warn, 4.5),
            // Not text: a fill, a rule or a focus ring, which WCAG puts at 3:1.
            ("focus ring on panel", c.accent, c.surface, 3.0),
        ] {
            let got = contrast(fg, bg);
            assert!(got >= min, "{theme}: {name}: contrast {got:.2} < {min}");
        }
    }
    // The script editor's code box is dark in both themes (`script_editor`
    // gives it the dark visuals), so its colours are measured once, on
    // the dark ink.
    let ink = DARK.ink;
    for (name, fg) in [
        ("code text", DARK.text),
        ("code keyword", CODE_KEYWORD),
        ("code string", CODE_STRING),
        ("code literal", CODE_LITERAL),
        ("code comment", CODE_COMMENT),
        ("code api", CODE_API),
        ("code call", CODE_CALL),
    ] {
        let got = contrast(fg, ink);
        assert!(got >= 4.5, "{name}: contrast {got:.2} < 4.5");
    }
}

/// Composite a translucent colour over an opaque one, as egui paints it.
///
/// Done on the [`Color32`] egui is handed, which stores its channels
/// **premultiplied** (`from_rgba_unmultiplied` multiplies on construction),
/// so the source term is already `rgb·a` and this is plain
/// `src + dst·(1 − a)`. The result is opaque.
fn over(fg: impl Into<Color32>, bg: impl Into<Color32>) -> Rgba8 {
    let (fg, bg): (Color32, Color32) = (fg.into(), bg.into());
    let inv = 1.0 - f32::from(fg.a()) / 255.0;
    let ch = |f: u8, b: u8| (f32::from(f) + f32::from(b) * inv).round() as u8;
    Rgba8::from_rgb(ch(fg.r(), bg.r()), ch(fg.g(), bg.g()), ch(fg.b(), bg.b()))
}

/// The basemap under every overlay box, at its brightest.
///
/// Not an adversarial extreme — it is *the* case. The imagery is a map, not
/// a satellite photo: pale beige land on near-white paper, which is the same
/// fact `globe::GLOBE_DEEP_ALBEDO` exists to deal with. A translucent chrome
/// box tested only against the panel fill it borrows its colour from has
/// been tested against the one background it never has.
const BRIGHT_MAP: Rgba8 = Rgba8::from_rgb(0xFF, 0xFF, 0xFF);
/// The same at the other end — a night city, a dark aerial, deep water.
const DARK_MAP: Rgba8 = Rgba8::from_rgb(0x18, 0x18, 0x18);

/// The panel is solid and cool, never warmer (the brown a mix of the brand
/// orange produced, red above blue, is what this pins against).
///
/// **Dark**, it is visibly teal rather than black, and the blocks in it are
/// close to it, since a jump in tone between the two was "too much
/// contrast" (2026-11-10): a block is told apart by its hairline and its
/// shadow. **Light** takes the hub's white cards on its page instead, a
/// step further apart (1.10:1), so the quiet-blocks rule is the dark
/// theme's.
#[test]
fn the_panel_is_a_dark_teal_with_quiet_blocks() {
    for (theme, c) in BOTH {
        let panel = c.panel;
        assert_eq!(panel.a(), 255, "{theme}: the panel is solid: {panel:?}");
        assert!(
            panel.r() <= panel.b() && panel.r() <= panel.g(),
            "{theme}: the panel has a warm cast: {panel:?}"
        );
    }
    assert!(
        DARK.panel.g() > DARK.panel.r() + 8,
        "the panel has gone black: {:?}",
        DARK.panel
    );
    assert!(
        contrast(DARK.card, DARK.panel) < 1.08,
        "the blocks jump off the panel: {}",
        contrast(DARK.card, DARK.panel)
    );
}

/// **Every surface a colour can land on, not just the panel fill.**
///
/// [`text_colours_clear_their_backgrounds`] pins the pairs against
/// [`Palette::surface`] and the accent fills. That is the easy half of the
/// palette: the ramp exists precisely so things *can* sit on the raised and
/// hover steps, and egui puts them there without being asked —
/// `weak_text_color` inside an `open` menu is the dim tone on the hover
/// step, which measured 4.40 and is the failure this test was written for.
///
/// The overlay is the other half, and it was worse, because its background
/// is not in this file at all: it is the map, at both ends of its
/// brightness. The boxes are solid now, so the composite is the fill; the
/// rows stay so a box made translucent again is measured over the map.
#[test]
fn text_clears_every_surface_it_can_land_on() {
    for (theme, c) in BOTH {
        let over_bright = over(c.overlay, BRIGHT_MAP);
        let over_dark = over(c.overlay, DARK_MAP);
        let glass_bright = over(c.glass, BRIGHT_MAP);
        let glass_dark = over(c.glass, DARK_MAP);
        for (name, fg, bg, min) in [
            // The raised step: buttons under the pointer, striped rows.
            ("dim on raised", c.text_dim, c.surface_raised, 4.5),
            ("heading on raised", c.cream, c.surface_raised, 4.5),
            ("link on raised", c.accent_text, c.surface_raised, 4.5),
            ("warning on raised", c.warn, c.surface_raised, 4.5),
            ("error on raised", c.error, c.surface_raised, 4.5),
            (
                "held-state rim on raised",
                c.accent_bright,
                c.surface_raised,
                4.5,
            ),
            // The glyph on an engaged plate, the one place in the dark
            // theme where dark type sits on a light fill.
            ("ink on an engaged plate", c.on_accent, c.accent, 4.5),
            (
                "ink on an engaged plate's rim",
                c.on_accent,
                c.accent_ring,
                4.5,
            ),
            // The hover step — and an *open* menu button sits here at rest.
            ("body on hover", c.text, c.surface_hover, 4.5),
            ("dim on hover", c.text_dim, c.surface_hover, 4.5),
            ("heading on hover", c.cream, c.surface_hover, 4.5),
            ("link on hover", c.accent_text, c.surface_hover, 4.5),
            ("error on hover", c.error, c.surface_hover, 4.5),
            // Text-edit wells, a segmented control's track.
            ("body on ink", c.text, c.ink, 4.5),
            ("dim on ink", c.text_dim, c.ink, 4.5),
            ("link on ink", c.accent_text, c.ink, 4.5),
            // The panels, and the cards in them.
            ("body on a panel", c.text, c.panel, 4.5),
            ("dim on a panel", c.text_dim, c.panel, 4.5),
            ("hairline on a panel", c.line, c.panel, 3.0),
            ("body on a card", c.text, c.card, 4.5),
            ("dim on a card", c.text_dim, c.card, 4.5),
            // The boxes over the map, against the map — both ends of it.
            (
                "body on an overlay over bright map",
                c.text,
                over_bright,
                4.5,
            ),
            (
                "dim on an overlay over bright map",
                c.text_dim,
                over_bright,
                4.5,
            ),
            (
                "heading on an overlay over bright map",
                c.cream,
                over_bright,
                4.5,
            ),
            (
                "link on an overlay over bright map",
                c.accent_text,
                over_bright,
                4.5,
            ),
            (
                "warning on an overlay over bright map",
                c.warn,
                over_bright,
                4.5,
            ),
            (
                "error on an overlay over bright map",
                c.error,
                over_bright,
                4.5,
            ),
            ("body on an overlay over dark map", c.text, over_dark, 4.5),
            (
                "dim on an overlay over dark map",
                c.text_dim,
                over_dark,
                4.5,
            ),
            // The panels and windows, over the map.
            ("body on a panel over bright map", c.text, glass_bright, 4.5),
            (
                "dim on a panel over bright map",
                c.text_dim,
                glass_bright,
                4.5,
            ),
            (
                "heading on a panel over bright map",
                c.cream,
                glass_bright,
                4.5,
            ),
            (
                "link on a panel over bright map",
                c.accent_text,
                glass_bright,
                4.5,
            ),
            (
                "warning on a panel over bright map",
                c.warn,
                glass_bright,
                4.5,
            ),
            (
                "error on a panel over bright map",
                c.error,
                glass_bright,
                4.5,
            ),
            ("body on a panel over dark map", c.text, glass_dark, 4.5),
            ("dim on a panel over dark map", c.text_dim, glass_dark, 4.5),
        ] {
            let got = contrast(fg, bg);
            assert!(got >= min, "{theme}: {name}: contrast {got:.2} < {min}");
        }
    }

    // The map's own labels are dark in both themes (`LABEL_CHIP`,
    // `LABEL_HALO`): a few words on an arbitrary tile, and the text on
    // them is the dark palette's.
    let chip_bright = over(LABEL_CHIP, BRIGHT_MAP);
    let halo_bright = over(LABEL_HALO, BRIGHT_MAP);
    let halo_dark = over(LABEL_HALO, DARK_MAP);
    for (name, fg, bg) in [
        ("chip label over bright map", DARK.cream, chip_bright),
        ("chip dim over bright map", DARK.text_dim, chip_bright),
        ("street name over bright map", LABEL_INK, halo_bright),
        ("street name over dark map", LABEL_INK, halo_dark),
    ] {
        let got = contrast(fg, bg);
        assert!(got >= 4.5, "{name}: contrast {got:.2} < 4.5");
    }
}

/// **The state ladder moves one way.** Rest → hover → pressed → engaged,
/// each a step further from the rest surface — lighter on the dark theme,
/// darker on the light one: lightness is the channel that still says "this
/// control is doing something" to someone who cannot see the hue, and a
/// ladder that doubles back is a UI where two different states look the
/// same.
#[test]
fn the_state_ladder_rises() {
    for (theme, c) in BOTH {
        let rungs = [
            ("rest", c.surface_raised),
            ("hover", c.surface_hover),
            ("pressed", c.accent_muted),
            ("engaged", c.accent),
        ];
        // +1 when the ladder climbs (dark), -1 when it descends (light).
        let way = (luminance(c.accent) - luminance(c.surface_raised)).signum();
        for pair in rungs.windows(2) {
            let ((lo_name, lo), (hi_name, hi)) = (pair[0], pair[1]);
            assert!(
                (luminance(hi) - luminance(lo)) * way > 0.0,
                "{theme}: {hi_name} ({:.3}) is not a step past {lo_name} ({:.3})",
                luminance(hi),
                luminance(lo)
            );
        }
        // And the two ends are far enough apart to be told apart at a
        // glance, which "strictly monotonic" alone does not promise.
        let got = contrast(c.accent, c.surface_raised);
        assert!(
            got >= 3.0,
            "{theme}: engaged against rest: contrast {got:.2} < 3.0"
        );
    }
}

/// **A control has to be findable before its label matters**, and WCAG puts
/// that boundary at 3:1 (1.4.11) — the same bar as a focus ring, for the same
/// reason: it is the only thing saying *here is a thing you can press*.
///
/// A button at rest is the raised step on the surface, about 1.2:1 of fill
/// difference, framed by a [`Palette::line`] hairline that used to measure
/// 1.92:1 on the dark theme. Neither channel identified the control.
///
/// Two pairs are deliberately *not* asserted: the hover step against the
/// raised one is a *change* of state rather than the identification of a
/// control, and the accent ring arriving with it is what carries hover; and
/// the line against the hover step only ever appears on an `open` menu
/// button, where the popup below it is the thing being looked at.
#[test]
fn a_control_is_distinguishable_from_the_surface_it_sits_on() {
    for (theme, c) in BOTH {
        let v = visuals_of(&Palette32::of(c));
        // A button at rest is edged in the hub's soft border, not a 3:1
        // line: the person called a page of 3:1-edged buttons "cheap"
        // (2026-11-10), and the hub tells its buttons by plate, shape and
        // word. The edge must still be there, so it is held to a floor.
        for (name, got) in [
            (
                "a button's border on a panel",
                contrast(v.widgets.inactive.bg_stroke.color, c.surface),
            ),
            (
                "a button's border on the page",
                contrast(v.widgets.inactive.bg_stroke.color, c.panel),
            ),
        ] {
            assert!(got >= 1.3, "{theme}: {name}: contrast {got:.2} < 1.3");
        }
        for (name, got) in [
            (
                "the hover ring against the fill it encloses",
                contrast(v.widgets.hovered.bg_stroke.color, c.surface_hover),
            ),
            (
                "the pressed ring against the fill it encloses",
                contrast(
                    v.widgets.active.bg_stroke.color,
                    v.widgets.active.weak_bg_fill,
                ),
            ),
            (
                "the pressed ring against the panel outside it",
                contrast(v.widgets.active.bg_stroke.color, c.surface),
            ),
            (
                "the engaged plate against the panel it sits on",
                contrast(c.accent, c.surface),
            ),
            (
                "the glyph on an engaged plate",
                contrast(c.on_accent, c.accent),
            ),
        ] {
            assert!(got >= 3.0, "{theme}: {name}: contrast {got:.2} < 3.0");
        }

        // **The focus ring is two rings and has to be checked as two.** It
        // is drawn *outside* the control, so its background is the map at
        // any brightness, or a panel, or the plate of the control it
        // encircles. `paint_focus_ring` answers that with the ink under
        // the strongest text tone — dark under light on the dark theme,
        // the other way round on the light one — and the property is that
        // **at least one of them clears 3:1 on every background**.
        for (name, bg) in [
            ("a panel", c.surface),
            (
                "an overlay disc over bright map",
                over(c.overlay, BRIGHT_MAP),
            ),
            ("an overlay disc over dark map", over(c.overlay, DARK_MAP)),
            ("the plate of an engaged disc", c.accent),
            ("bare bright basemap", BRIGHT_MAP),
            ("bare dark basemap", DARK_MAP),
        ] {
            let best = contrast(c.cream, bg).max(contrast(c.ink, bg));
            assert!(
                best >= 3.0,
                "{theme}: the keyboard-focus ring on {name}: neither half clears \
                     3:1 (best {best:.2})"
            );
        }
    }
}

/// The palette's own prose is a *claim*, and prose does not recompute.
///
/// Three numbers in this file were wrong when this was written, all about
/// the same pair and none checkable by reading: the ink on the old accent
/// was written as 5.9 in the module note and 6.0 further down, and the
/// alternative it was compared against — "only 3.1:1" — was
/// [`Color32::WHITE`]'s ratio, not cream's. Pin the ratios the file
/// commits to in writing (the dark theme's, which the prose quotes), so
/// editing a colour and not the paragraph beside it is a failing test
/// rather than a stale comment.
#[test]
fn the_documented_ratios_are_the_measured_ones() {
    let c = &DARK;
    for (claim, fg, bg, stated) in [
        ("text on surface", c.text, c.surface, 14.3),
        ("text_dim on surface", c.text_dim, c.surface, 9.4),
        ("accent_text on surface", c.accent_text, c.surface, 17.6),
        ("accent on surface", c.accent, c.surface, 3.9),
        ("on_accent on accent", c.on_accent, c.accent, 4.5),
        ("cream on accent_muted", c.cream, c.accent_muted, 10.5),
        ("line on surface", c.line, c.surface, 3.8),
        ("accent_lift label", c.on_accent, c.accent_lift, 5.1),
        ("accent_ring label", c.on_accent, c.accent_ring, 5.8),
        (
            "on_accent on accent_pressed",
            c.on_accent,
            c.accent_pressed,
            5.8,
        ),
    ] {
        // Compare at the precision the prose actually claims: the docs say
        // "11.9:1", so the test that keeps them honest is whether the
        // measurement *rounds* to that, not whether it equals it.
        let got = contrast(fg, bg);
        assert!(
            (got * 10.0).round() / 10.0 == stated,
            "{claim}: the docs say {stated:.1}:1, the colours measure {got:.2}:1"
        );
    }
}

/// egui derives three app-wide text colours from the widget table rather
/// than exposing them directly — plain text from `noninteractive`, weak text
/// from `weak_text_color`, and **`RichText::strong()` from `widgets.active`**.
/// That last one is the trap: `active` is also the pressed-button state, so
/// picking its `fg_stroke` to suit the fill silently recolours every bold
/// value in the app. Assert the three land where the palette says, in both
/// themes.
#[test]
fn derived_text_colours_come_from_the_palette() {
    for (theme, c) in BOTH {
        let v = visuals_of(&Palette32::of(c));
        assert_eq!(v.text_color(), c.text.into(), "{theme}");
        assert_eq!(v.weak_text_color(), c.text_dim.into(), "{theme}");
        assert_eq!(v.strong_text_color(), c.cream.into(), "{theme}");
        assert_eq!(v.dark_mode, c.dark, "{theme}");
    }
}

/// The dark palette is the look the person signed off, value for value:
/// the light theme was added beside it and moved none of it. The accent
/// plates went a deeper teal with white ink on 2026-11-10, at the
/// person's word; anything else that moves here should be asked for too.
#[test]
fn the_dark_palette_is_the_look_it_was() {
    let rgb = |c: Rgba8| (c.r(), c.g(), c.b(), c.a());
    assert_eq!(rgb(DARK.ink), (0x0F, 0x14, 0x16, 255));
    assert_eq!(rgb(DARK.surface), (0x14, 0x1A, 0x1C, 255));
    assert_eq!(rgb(DARK.surface_raised), (0x1F, 0x28, 0x29, 255));
    assert_eq!(rgb(DARK.surface_hover), (0x2A, 0x35, 0x36, 255));
    assert_eq!(rgb(DARK.line), (0x63, 0x79, 0x7C, 255));
    assert_eq!(rgb(DARK.accent), (0x2B, 0x83, 0x7E, 255));
    assert_eq!(rgb(DARK.accent_ring), (0x24, 0x70, 0x6C, 255));
    assert_eq!(rgb(DARK.accent_pressed), (0x24, 0x70, 0x6C, 255));
    assert_eq!(rgb(DARK.accent_muted), (0x26, 0x3F, 0x42, 255));
    assert_eq!(rgb(DARK.cream), (0xF4, 0xF8, 0xF8, 255));
    assert_eq!(rgb(DARK.text), (0xE2, 0xE9, 0xEA, 255));
    assert_eq!(rgb(DARK.text_dim), (0xAA, 0xC2, 0xC6, 255));
    assert_eq!(rgb(DARK.warn), (0xD8, 0xC0, 0x7A, 255));
    assert_eq!(rgb(DARK.error), (0xF2, 0xA2, 0x96, 255));
    assert_eq!(rgb(DARK.panel), (0x17, 0x23, 0x24, 255));
    assert_eq!(rgb(DARK.card), (0x16, 0x1F, 0x21, 255));
    assert_eq!(rgb(DARK.panel_rim), (0x2A, 0x35, 0x36, 255));
    assert_eq!(
        DARK.on_status, DARK.ink,
        "a status plate is labelled in ink"
    );
    for white in [DARK.on_accent, DARK.accent_bright, DARK.accent_text] {
        assert_eq!(white, Rgba8::WHITE);
    }
    assert_eq!(rgb(DARK.accent_lift), (0x27, 0x7A, 0x75, 255));
    assert_eq!(rgb(DARK.accent_word), (0xA8, 0xDC, 0xD8, 255));
    assert_eq!((DARK.overlay, DARK.glass), (DARK.panel, DARK.panel));
    assert_eq!(DARK.overlay_line, DARK.panel_rim);
    assert_eq!(DARK.shadow, 1.0);
    // The map's round buttons in the dark theme are what they were: the
    // light theme's teal identity (2026-11-10) did not move them.
    assert_eq!(
        (DARK.map_fill, DARK.map_ink, DARK.map_rim, DARK.map_mark),
        (DARK.overlay, DARK.text, DARK.overlay_line, DARK.cream)
    );
}

/// The map's round buttons at rest, in both themes: the glyph reads on its
/// plate as text does (4.5:1), and in the light theme the plate is white
/// with a teal glyph — teal visibly the brand colour, not only on the one
/// plate that is on (2026-11-10).
#[test]
fn the_map_buttons_wear_the_teal_in_both_themes() {
    for pal in [&DARK, &LIGHT] {
        let c = contrast(pal.map_ink, pal.map_fill);
        assert!(c >= 4.5, "map glyph on its plate is {c:.2}:1");
        let m = contrast(pal.map_mark, pal.map_fill);
        assert!(m >= 3.0, "the rail's thumb on a plate is {m:.2}:1");
    }
    assert_eq!(LIGHT.map_fill, Rgba8::WHITE);
    // Teal: green and blue well above red, in the glyph and the rim.
    for teal in [LIGHT.map_ink, LIGHT.map_rim, LIGHT.map_mark] {
        assert!(
            teal.g() > teal.r() + 40 && teal.b() > teal.r() + 40,
            "{teal:?}"
        );
    }
}

/// The wordmark is the app's name split in two, and nothing else. It is
/// written in three places — here, `index.html`'s loader and the account
/// GUI's header — so the halves are exactly where a typo would ship as a
/// misspelt product name that still looks deliberate.
#[test]
fn the_wordmark_halves_spell_the_app() {
    assert_eq!(format!("{WORDMARK_HEAD}{WORDMARK_TAIL}"), "cartopolis");
}

/// The pin is one `convex_polygon`, and epaint fills a polygon that is not
/// convex wrongly rather than refusing it — a teardrop regenerated with a
/// dent would paint as a wedge with nothing to say why.
#[test]
fn the_pin_outline_is_convex() {
    let o = &mark::PIN_OUTLINE;
    let mut sign = 0.0_f32;
    for i in 0..o.len() {
        let (a, b, c) = (o[i], o[(i + 1) % o.len()], o[(i + 2) % o.len()]);
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        if cross.abs() < 1e-3 {
            continue;
        }
        assert!(
            sign == 0.0 || sign == cross.signum(),
            "the pin outline turns the other way at point {i}"
        );
        sign = cross.signum();
    }
}

/// Nothing in the plan reaches outside the tile or into the pin's halo. The
/// mark is painted with no clip of any kind — a `Painter` at the sign-in
/// card's 22 pt will happily draw a street across the card's own text — so
/// the generator's cut is the only clip there is.
#[test]
fn the_plan_stays_inside_the_tile_and_clear_of_the_pin() {
    let square = 2.0 * mark::UNIT;
    let o = &mark::PIN_OUTLINE;
    let to_pin = |p: [f32; 2]| {
        (0..o.len())
            .map(|i| {
                let (a, b) = (o[i], o[(i + 1) % o.len()]);
                let d = [b[0] - a[0], b[1] - a[1]];
                let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1])
                    / (d[0] * d[0] + d[1] * d[1]))
                    .clamp(0.0, 1.0);
                (p[0] - a[0] - t * d[0]).hypot(p[1] - a[1] - t * d[1])
            })
            .fold(f32::INFINITY, f32::min)
    };
    assert!(!mark::LINES.is_empty(), "the plan is empty");
    for (i, line) in mark::LINES.iter().enumerate() {
        for &p in line.points {
            assert!(
                (0.0..=square).contains(&p[0]) && (0.0..=square).contains(&p[1]),
                "line {i} reaches ({}, {}), outside the tile",
                p[0],
                p[1]
            );
            // One unit of slack for the generator's rounding to whole units.
            assert!(
                to_pin(p) >= mark::PIN_HALO + line.width / 2.0 - 1.0,
                "line {i} runs into the pin's halo at ({}, {})",
                p[0],
                p[1]
            );
        }
    }
}

/// The brand family must be bound before anything paints a heading —
/// `install` exists to guarantee that, and an unbound family is a panic deep
/// in epaint rather than a missing-font fallback.
#[test]
fn install_binds_the_brand_family_for_the_first_frame() {
    let ctx = egui::Context::default();
    install(&ctx);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
    let _ = ctx.run_ui_unpainted(
        egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        },
        |ui| {
            ui.heading("Places");
        },
    );
}

/// The two brand primitives paint, on the first pass, into a bare context.
///
/// Cheap, and it covers the one way this could fail at runtime rather than at
/// compile time: [`paint_wordmark`] lays its galleys out in
/// [`brand_family`], and an unbound family is a panic deep in epaint, not a
/// fallback. The splash paints on frame one — there is no second chance to
/// find that out.
#[test]
fn the_mark_and_the_wordmark_paint_on_the_first_pass() {
    let ctx = egui::Context::default();
    install(&ctx);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
    let out = ctx.run_ui_unpainted(
        egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        },
        |ui| {
            let p = ui.painter();
            paint_mark(p, egui::pos2(200.0, 100.0), 60.0, 1.0);
            paint_wordmark(p, egui::pos2(200.0, 220.0), SPLASH_WORDMARK, 1.0);
        },
    );
    // The tile, the street plan, the pin and its dot, and the wordmark's
    // two galleys.
    assert!(
        out.shapes.len() >= 30,
        "the brand painted only {} shapes",
        out.shapes.len()
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
        1 => P::Search,
        2 => P::Layers,
        3 => P::Places,
        4 => P::Route,
        5 => P::Chat,
        6 => P::Appearance,
        7 => P::Script,
        8 => P::Models,
        9 => P::Settings,
        10 => P::About,
        11 => P::Admin,
        _ => return None,
    };
    let back = match panel {
        P::None => 0,
        P::Search => 1,
        P::Layers => 2,
        P::Places => 3,
        P::Route => 4,
        P::Chat => 5,
        P::Appearance => 6,
        P::Script => 7,
        P::Models => 8,
        P::Settings => 9,
        P::About => 10,
        P::Admin => 11,
    };
    assert_eq!(back, i, "panel_at's two tables disagree at {i}");
    Some(panel)
}

/// No panel may wear a glyph the font cannot draw.
///
/// Kept after the icon font landed, and narrower than
/// `icons::tests::every_icon_has_a_glyph`: that one checks the table, this
/// one checks that each panel is actually *wired* to a row of it. A panel
/// added with a hand-typed glyph — which is how `🧭`, `🤖` and `🧍` shipped as
/// empty boxes, twice over once the header started repeating the dock's icon
/// — fails here rather than on somebody's screen.
#[test]
fn every_panel_icon_resolves_to_a_glyph_the_font_has() {
    let ctx = egui::Context::default();
    install(&ctx);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
    let _ = ctx.run_ui_unpainted(
        egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        },
        |ui| {
            let mut seen = 0;
            while let Some(panel) = panel_at(seen) {
                seen += 1;
                let icon = panel.icon();
                let font = TextStyle::Body.resolve(ui.style());
                assert!(
                    ui.ctx().fonts_mut(|f| f.has_glyphs(&font, icon)),
                    "{}: wears {icon:?}, which the font cannot draw",
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
/// preference lists (see [`literals_of`]) already accounted for.
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

    // A bracketed run of nothing but string literals is a preference list —
    // the shape `first_drawable` took before the icon font retired it, and
    // one anybody may write again for a non-icon glyph. Its whole point is
    // that the earlier entries may be missing and only the last has to
    // resolve, so the scan skips all but the last.
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
        // A module's `tests.rs` is its `#[cfg(test)] mod tests`, which the
        // scan leaves out where it is written inline.
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("tests.rs") {
            out.push(path);
        }
    }
}

/// **Nothing the app paints may be a glyph the font lacks.**
///
/// The icons this rule was written for are gone from the source: they live
/// in [`crate::systems::icons`] now, as constants backed by a font the app
/// ships, and `icons::tests::every_icon_has_a_glyph` is what checks *those*.
/// This test still matters for everything else — a `≈` in a readout, a `°`,
/// the next symbol somebody types straight into a label — which is exactly
/// the case that shipped broken twice: the HUD's `⧉` copy button and the
/// furniture prompt's `🪑`, neither visible in a review, a `cargo check` or a
/// screenshot nobody happens to take of that corner.
///
/// So the test does what a reviewer cannot: it reads the source, takes every
/// short string literal that is not part of a preference list, and asks the
/// *loaded font stack* whether each non-Latin-1 character in it has a glyph.
/// Latin-1 is exempt because Ubuntu-Light covers it and a `°` or a `·` is not
/// a guess anyone is making.
/// The name is painted in one orange on both halves of the startup screen.
///
/// `systems::splash` and `index.html`'s loader are one screen handed over
/// mid-load, and the module doc has said "change one and change the other"
/// since they were built. They drifted anyway, silently and in the way that
/// rule cannot catch: the Rust side used `ACCENT_TEXT` (`#FFA470`) and the CSS
/// used `#FF9457`, two different lightened accents, neither of them the plain
/// `#FD6012` of the mark's ring right above them and the progress bar right
/// below. Nobody sees the handover as a colour change — the wordmark simply
/// reads pink on a screen whose every other orange is the brand's.
///
/// Reading the real `index.html` rather than restating the hex here: a
/// constant copied into a test is a second place to drift, and this is a test
/// about two places already having done that.
#[test]
fn the_wordmark_is_one_orange_on_both_splashes() {
    assert_eq!(
        WORDMARK_TAIL_COLOR, BRAND,
        "the wordmark tail is the brand orange — not the chrome's accent, \
             which is the teal"
    );
    let css = format!(
        "#{:02x}{:02x}{:02x}",
        WORDMARK_TAIL_COLOR.r(),
        WORDMARK_TAIL_COLOR.g(),
        WORDMARK_TAIL_COLOR.b()
    );
    let loader = include_str!("../../../../index.html");
    let rule = loader
        .split("#wordmark b {")
        .nth(1)
        .expect("index.html still styles the wordmark tail")
        .split('}')
        .next()
        .expect("`#wordmark b` rule is closed");
    assert!(
        rule.to_ascii_lowercase().contains(&css),
        "index.html paints the wordmark tail {rule:?}, but the app paints it {css}"
    );
}

#[test]
fn the_emoji_people_type_in_chat_are_drawn() {
    // Chat text is whatever people type, so no ladder can stand in for a
    // missing glyph there. 🙂 and 🕺 drew as `◻` on the phone until the
    // current Noto Emoji joined the stack (`EMOJI_TTF`).
    let ctx = egui::Context::default();
    install(&ctx);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
    let _ = ctx.run_ui_unpainted(
        egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        },
        |ui| {
            let font = TextStyle::Body.resolve(ui.style());
            for ch in ['🙂', '🕺', '🤖', '🛒', '👍', '🥳', '🫠'] {
                let width = ui.ctx().fonts_mut(|f| f.glyph_width(&font, ch));
                assert!(width > 0.0, "{ch} (U+{:X}) draws as a box", ch as u32);
            }
        },
    );
}

#[test]
fn no_painted_glyph_is_tofu() {
    // **Both crates that put a glyph on screen, not just this one.**
    // `cartopolis_geo` used to own two hard-coded icon tables —
    // `PoiCategory::glyph` and `TravelMode::icon` — whose output was painted
    // straight into the interact prompt and the route panel. Scanning only
    // this crate's `src` let `🛍` (Shop) and `🛏` (Lodging) ship as empty
    // boxes for as long as they existed: the rule was right and its reach
    // stopped at a crate boundary. Those tables moved to `systems::icons`
    // with the icon font, but the reach is kept — a label written over there
    // is painted over here just the same, and `cartopolis_geo` cannot depend
    // on egui, so it cannot host this test itself.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let roots = [manifest.join("src"), manifest.join("../geo/src")];
    let mut files = Vec::new();
    for root in &roots {
        source_files(root, &mut files);
    }
    assert!(
        files.len() > 20,
        "found only {} source files under {:?} — the walk is broken, not the tree",
        files.len(),
        roots
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
    let _ = ctx.run_ui_unpainted(
        egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        },
        |ui| {
            let font = TextStyle::Body.resolve(ui.style());
            // The width test below must still see a character nothing
            // draws, or it passes everything.
            assert_eq!(
                ui.ctx().fonts_mut(|f| f.glyph_width(&font, '\u{10FFFD}')),
                0.0,
                "a character no font has must measure 0 wide"
            );
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
                    // `has_glyphs` answers "not the replacement face", and
                    // the face epaint draws `◻` from is Noto Emoji — so every
                    // emoji, though Noto Emoji draws it, read as missing. A
                    // character no face has measures 0 wide; one the
                    // replacement face really has does not.
                    let drawable = ui.ctx().fonts_mut(|f| {
                        f.has_glyphs(&font, &glyph) || f.glyph_width(&font, ch) > 0.0
                    });
                    if !drawable {
                        missing.push((lit.file.clone(), lit.line, ch));
                    }
                }
            }

            let excused =
                |file: &str, ch: char| KNOWN_TOFU.iter().any(|(f, c, _)| *f == file && *c == ch);
            let unexcused: Vec<String> = missing
                .iter()
                .filter(|(file, _, ch)| !excused(file.as_str(), *ch))
                .map(|(file, line, ch)| format!("{file}:{line} draws U+{:04X} {ch:?}", *ch as u32))
                .collect();
            assert!(
                unexcused.is_empty(),
                "the font cannot draw these, so they paint as empty boxes — if it is \
                     an icon, take it from `systems::icons`; otherwise use something \
                     Latin-1, which every font here covers:\n  {}",
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
        ("panel title", SIZE_PANEL_TITLE),
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
    // A desktop — including one that forced the on-screen keyboard on with
    // `?osk` — magnifies nothing, so a point is a dp.
    assert!((dp_per_point(1.0, 1.0, false) - 1.0).abs() < 1e-6);
    // And the keyboard's own floor, which is the tightest consumer of the
    // pair: 48 dp at `ui_scale` 1.5 is a 32 pt key.
    assert!((min_touch_target_pts(android) - 32.0).abs() < 0.01);
    let web_key = min_touch_target_pts(web);
    assert!(web_key > 25.0 && web_key < 27.0, "web key {web_key} pt");
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
        // …and why [`action`] refuses `small()` under a finger. egui's small
        // button zeroes the vertical padding and drops to the `Small` text
        // style, so its height *is* the glyph — a third of the row it sits
        // in, on the controls that exist to save you typing an address on a
        // phone.
        let small_dp = style_for(true).text_styles[&TextStyle::Small].size * dp_per_pt;
        assert!(
            small_dp < MIN_TOUCH_TARGET_DP,
            "{name}: a small button is {small_dp:.0} dp, so a small egui button \
                 has nothing left to protect against"
        );
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
