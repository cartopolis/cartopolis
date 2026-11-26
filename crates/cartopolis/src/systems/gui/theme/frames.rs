use super::*;

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
        .fill(p32().overlay)
        .stroke(Stroke::new(1.0, p32().overlay_line))
        .corner_radius(CornerRadius::same(RADIUS_LG))
        .inner_margin(Margin::symmetric(14, 12))
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
pub fn chip_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(LABEL_CHIP.to_egui())
        .corner_radius(CornerRadius::same(RADIUS_SM))
        .inner_margin(Margin::symmetric(
            CHIP_PADDING.x as i8,
            CHIP_PADDING.y as i8,
        ))
}

/// The design's floating card (`.float-top`, `.sheet`): solid since
/// 2026-11-08 (it was ink at 84 %), a hairline, round corners.
pub fn glass_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(p32().glass)
        .stroke(egui::Stroke::new(1.0, p32().panel_rim))
        .corner_radius(CornerRadius::same(20))
        .inner_margin(Margin::symmetric(12, 10))
}

/// Small text over the map, legible on both white paper and dark water.
///
/// The halo idiom is the scale bar's, and for its reason: a filled plate behind
/// something this small reads as a piece of chrome that has come loose. Four
/// offset copies rather than a stroke, because epaint's text has no outline —
/// diagonals as well as axes, or the halo thins to nothing at the corners of a
/// glyph. Anchored by `align` at `at`: the locate caption ends at its disc's
/// right edge, the hand button's word sits to the left of its disc
/// (`map_osd`).
pub fn halo_text_aligned(
    painter: &egui::Painter,
    at: egui::Pos2,
    align: egui::Align2,
    text: &str,
    ink: egui::Color32,
) {
    let font = egui::FontId::proportional(SIZE_TINY);
    for dx in [-1.0, 0.0, 1.0] {
        for dy in [-1.0, 0.0, 1.0] {
            if dx == 0.0 && dy == 0.0 {
                continue;
            }
            painter.text(
                at + egui::vec2(dx, dy),
                align,
                text,
                font.clone(),
                p32().overlay,
            );
        }
    }
    painter.text(at, align, text, font, ink);
}

/// Height of a primary button on a pointer. Under a finger it grows to
/// [`row_height`], which is larger.
pub const PRIMARY_BUTTON_HEIGHT: f32 = 40.0;

/// **The** call to action on a screen: filled with the accent, labelled in
/// [`Palette::ink`] at [`SIZE_VALUE`], taller than an ordinary button, and at least
/// `min_width` points wide.
///
/// There should be at most one per screen, and it should be the thing you came to
/// do — Sign in, Start driving. Everything else is `ui.button`.
///
/// Exists because the app already had two primary buttons that did not look
/// alike: the sign-in CTA was a hand-built filled `Button` and the Route panel's
/// equally-primary "▶ Driver mode" was a plain `ui.button`, so the same rank read
/// as two different ones. `INK` on `ACCENT` and not white is the palette's rule for
/// text on the accent (8.3:1 against 2.2:1) and is unit-tested below.
///
/// The width is always stated by the caller. A no-width `primary_button` wrapper
/// over a `PRIMARY_BUTTON_MIN_WIDTH` constant shipped beside this and never grew
/// a caller — both CTAs in the app fill the card they sit in — so it went rather
/// than sit behind an `allow(dead_code)` hiding the next genuinely dead thing.
///
/// **The label is centred by two growable atoms, not by the surrounding layout.**
/// egui 0.35 takes a button's atom alignment from `ui.layout()` — and a top-down
/// `Ui`, which is what every card, panel and dialog in this app hands down, is
/// `Align::Min`. So a button whose `min_size` is wider than its text put that
/// text against the left edge: the sign-in card's full-width CTA read as an
/// orange bar with a word stuck in the corner. An empty [`egui::Atom::grow`]
/// either side is the fix that does not touch where the *button* itself sits,
/// which the alternative — wrapping it in a `Align::Center` layout — would,
/// centring a narrow button in its row as a side effect.
///
/// **And it answers the pointer.** The fill was a flat [`Palette::accent`] in every
/// state, so the one control on the screen whose whole job is to be pressed gave
/// nothing back when you pressed it — no hover, no press, no focus ring, on a
/// button that then waits on a network round trip. It lifts to
/// [`Palette::accent_bright`] under the pointer, sinks to [`Palette::accent_pressed`] while held,
/// and wears an [`Palette::accent_ring`] outline when the keyboard is on it. The state
/// comes from the *previous* pass's response, read under the id this button is
/// about to take — the same thing egui's own `Button` does internally, and the
/// reason `next_auto_id` is safe here is that it reports the next id without
/// consuming it.
pub fn primary_button_sized(ui: &mut egui::Ui, label: &str, min_width: f32) -> egui::Response {
    let height = PRIMARY_BUTTON_HEIGHT.max(row_height());
    let was = ui.ctx().read_response(ui.next_auto_id());
    let held = was
        .as_ref()
        .is_some_and(egui::Response::is_pointer_button_down_on);
    let hovered = was.as_ref().is_some_and(egui::Response::hovered);
    let focused = was.as_ref().is_some_and(egui::Response::has_focus);
    let fill = if !ui.is_enabled() {
        p32().accent_muted
    } else if held {
        p32().accent_pressed
    } else if hovered {
        p32().accent_lift
    } else {
        p32().accent
    };
    // Depth (see `lift_shadow`): the accent's glow under it and a lit top edge,
    // both gone while it is held — a pressed key is down in the glass. The glow
    // has to go *under* the button, so its slot is reserved before the button
    // paints and filled once the rect is known.
    let lit = ui.is_enabled() && !held;
    let glow_slot = ui.painter().add(egui::Shape::Noop);
    let resp = ui.add(
        egui::Button::new((
            egui::Atom::grow(),
            egui::RichText::new(label)
                .size(SIZE_VALUE)
                .strong()
                .color(p32().on_accent),
            egui::Atom::grow(),
        ))
        .fill(fill)
        .stroke(if focused {
            Stroke::new(FOCUS_RING, p32().accent_ring)
        } else {
            Stroke::NONE
        })
        .corner_radius(CornerRadius::same(RADIUS))
        .min_size(egui::vec2(min_width, height)),
    );
    // Flat, as the midas hub's primary: the small shadow every button wears,
    // no glow and no lit edge.
    if lit {
        ui.painter().set(
            glow_slot,
            segment_shadow().as_shape(resp.rect, CornerRadius::same(RADIUS)),
        );
    }
    resp
}

/// The button beside a [`primary_button_sized`]: as tall, on a lit surface
/// with a hairline, the icon in the accent and the word in [`Palette::cream`].
///
/// It replaces two things that each read wrong in a card: an
/// [`action_button`], which is an outline pill the size of a line of text and
/// so too small to aim at, and a [`round_button`], whose fill is the map's
/// translucent overlay and is nearly black on a panel. An empty `label` makes
/// it an icon-only square when `min_width` is the height.
pub fn secondary_button(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    min_width: f32,
) -> egui::Response {
    secondary_toggle(ui, icon, label, min_width, false)
}

/// [`secondary_button`] for a switch: `on` wears the accent's tint and ring.
pub fn secondary_toggle(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    min_width: f32,
    on: bool,
) -> egui::Response {
    // Painted rather than an `egui::Button`: a button lays its atoms out with
    // padding and gaps of its own, so an icon-only one came out wider than the
    // square asked for, and a row of them pushed the panel wider every frame.
    // Here the size asked for is the size drawn.
    let height = PRIMARY_BUTTON_HEIGHT.max(row_height());
    let gap = 6.0;
    let mut icon_g = (!icon.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(icon.to_string(), FontId::proportional(17.0), p32().accent)
    });
    let word_g = (!label.is_empty()).then(|| {
        ui.painter().layout_no_wrap(
            label.to_string(),
            FontId::new(SIZE_BODY, brand_family()),
            p32().cream,
        )
    });
    let content = |icon: &Option<std::sync::Arc<egui::Galley>>| {
        let i = icon.as_ref().map_or(0.0, |g| g.size().x);
        let w = word_g.as_ref().map_or(0.0, |g| g.size().x);
        i + w + if i > 0.0 && w > 0.0 { gap } else { 0.0 }
    };
    // A button sharing a row (`equal_width`) gives up room before it widens
    // past its share: the padding first, then the icon, keeping the word. A
    // row that widened instead pushed the whole panel past a phone's edge —
    // the community page's People · Topic · 5 · Edit took 290 pt of a 250 pt
    // panel, and every card below it was cut off on the right (2026-10-31).
    let mut pad = 12.0;
    if word_g.is_some() && min_width > 0.0 && content(&icon_g) + 2.0 * pad > min_width {
        pad = 6.0;
        if content(&icon_g) + 2.0 * pad > min_width {
            icon_g = None;
        }
    }
    let content = content(&icon_g);
    let width = if word_g.is_some() {
        min_width.max(content + 2.0 * pad)
    } else {
        min_width.max(height)
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    // An icon-only one (`label` empty) is named again by its caller, from its
    // tooltip: a later `widget_info` replaces this one.
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, label)
    });
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let enabled = ui.is_enabled();
    let held = resp.is_pointer_button_down_on();
    // The midas hub's button (`m-button`): a dark surface with a hairline and
    // a small shadow, the word and its icon in the strong text tone. Under the
    // pointer it lifts a step and the hairline takes the accent; "on" is the
    // hub's soft teal tag. The hairline is [`Palette::control_edge`], the hub's
    // soft border a step firmer.
    let (fill, line, ink) = if !enabled {
        (p32().surface, p32().surface_hover, p32().cream)
    } else if on {
        (p32().accent_muted, p32().accent_muted, p32().accent_word)
    } else if held {
        (p32().surface_hover, p32().line, p32().cream)
    } else if resp.hovered() {
        (p32().surface_raised, p32().accent, p32().cream)
    } else {
        (p32().surface, p32().control_edge, p32().cream)
    };
    let p = ui.painter();
    if enabled && !held && !on {
        p.add(segment_shadow().as_shape(rect, CornerRadius::same(RADIUS)));
    }
    p.rect(
        rect,
        CornerRadius::same(RADIUS),
        fill,
        Stroke::new(1.0, line),
        egui::StrokeKind::Inside,
    );
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.5) };
    let mut x = rect.center().x - content / 2.0;
    if let Some(g) = icon_g {
        let w = g.size().x;
        p.galley_with_override_text_color(
            egui::pos2(x, rect.center().y - g.size().y / 2.0),
            g,
            dim(ink),
        );
        x += w + gap;
    }
    if let Some(g) = word_g {
        let y = rect.center().y - g.size().y / 2.0;
        p.galley_with_override_text_color(egui::pos2(x, y), g, dim(ink));
    }
    paint_focus_ring(ui, &resp, rect, false);
    resp
}

/// The width each of `n` buttons gets to share a row equally: a row of
/// controls that are one choice each reads as a set, sized to their words it
/// reads as a pile.
pub fn equal_width(ui: &egui::Ui, n: usize) -> f32 {
    let n = n.max(1) as f32;
    ((ui.available_width() - ui.spacing().item_spacing.x * (n - 1.0)) / n).floor()
}

/// Diameter of a round icon button, in points. Matches the compass disc in the
/// opposite corner — the map's floating controls are one family. Already 60 dp on
/// the touch tier, so it needs no finger-sized variant.
pub const ROUND_BUTTON_SIZE: f32 = 40.0;

/// Width of the ring that says "the keyboard is here".
pub(super) const FOCUS_RING: f32 = 2.0;

/// Draw the keyboard-focus ring *outside* `rect`, if this widget has focus.
///
/// The four controls in this file that are painted rather than assembled from
/// `Button` all branched on `hovered()` and `is_pointer_button_down_on()` only —
/// and they allocate with `Sense::click()`, which is focusable, so Tab lands on
/// them and nothing shows it. A keyboard user tabbing the dock had no way to
/// know where they were.
///
/// **Two rings, dark under light**, because this one is drawn *outside* the
/// control and therefore lands on whatever is behind it — which for the dock and
/// the compass is the map, at any brightness the day and the imagery can produce.
/// A single [`Palette::cream`] ring is 16.7:1 over the panel and invisible over white
/// paper; a single [`Palette::ink`] one is the reverse. The pair cannot both disappear, so
/// the outline is legible everywhere without the palette having to guess what is
/// under it. It used to be the light ring alone, and the monochrome chrome makes
/// that worse rather than better: there is no longer a saturated colour to fall
/// back on.
///
/// [`Palette::cream`] and not the accent for the inner ring: an accent ring is the
/// *pressed* state, and would say the wrong thing.
pub(super) fn paint_focus_ring(
    ui: &egui::Ui,
    resp: &egui::Response,
    rect: egui::Rect,
    round: bool,
) {
    if !resp.has_focus() {
        return;
    }
    let p = ui.painter();
    let ring = rect.expand(FOCUS_RING);
    // Outer first, wider, so the light ring is drawn over the middle of it and
    // what is left either side is a dark edge on both flanks.
    for (width, colour) in [
        (FOCUS_RING * 2.0, self::p32().ink),
        (FOCUS_RING, self::p32().cream),
    ] {
        let stroke = Stroke::new(width, colour);
        if round {
            p.circle_stroke(ring.center(), ring.width() * 0.5, stroke);
        } else {
            p.rect_stroke(
                ring,
                CornerRadius::same(RADIUS_SM),
                stroke,
                egui::StrokeKind::Outside,
            );
        }
    }
}

/// A round icon button that floats over the map: the dock along the bottom, and
/// anything else that wants the compass's visual language.
///
/// Painted rather than assembled from `Button`, because egui's button is a
/// rounded *rect* whose corner radius is capped well below half its height — a
/// "circular" button built that way is a squircle that reads as a mistake next to
/// the genuinely round compass. `selected` is the open-panel state and wears the
/// engaged plate — [`Palette::accent`] with [`Palette::ink`] on it, the same inversion every "this
/// is on" control in the app uses.
pub fn round_button(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    selected: bool,
) -> egui::Response {
    round_button_on(ui, icon, tooltip, selected, (p32().map_fill, p32().map_rim))
}

pub(super) fn round_button_on(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    selected: bool,
    rest: (Color32, Color32),
) -> egui::Response {
    let size = egui::Vec2::splat(ROUND_BUTTON_SIZE);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = named(ui, resp, tooltip);
    let hovered = resp.hovered();
    let (fill, ring, ink) = match (selected, hovered) {
        (true, _) => (p32().accent, p32().accent_ring, p32().on_accent),
        (false, true) => (p32().surface_hover, p32().accent, p32().cream),
        (false, false) => (rest.0, rest.1, p32().map_ink),
    };
    // Under the finger the disc gives a little (`motion::press`): the one
    // acknowledgement a painted control can make before its action shows.
    let press = crate::systems::motion::press(&resp);
    paint_disc(
        ui.painter(),
        rect.center(),
        (ROUND_BUTTON_SIZE * 0.5 - 1.0) * press,
        Disc {
            fill,
            ring,
            ink,
            icon,
            icon_size: 17.0 * press,
            ring_width: 1.5,
        },
    );
    paint_focus_ring(ui, &resp, rect, true);
    resp.on_hover_text(tooltip)
}

/// Give a painted control its name, as egui's own `Button` gives its text.
///
/// A control allocated with `allocate_exact_size` and painted by hand is,
/// to egui, a clickable rectangle with no name: screen readers and the MCP
/// `ui_tree` (`mcp::screen`) saw the dock and the map controls as sixteen
/// unlabelled areas. The name goes into egui's widget info, nowhere visible.
pub fn named(ui: &egui::Ui, resp: egui::Response, label: &str) -> egui::Response {
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    resp
}

/// Hang-up red: the one control in the app that is red at rest, because
/// ending a call is the action every phone draws in it, and a person reaching
/// for it in a hurry looks for the colour before the shape.
pub const CALL_END_RED: Rgba8 = Rgba8::from_rgb(0xD6, 0x45, 0x45);

/// Answer green: the other half of [`CALL_END_RED`], on the one screen where
/// both appear — a call coming in.
pub const CALL_ACCEPT_GREEN: Rgba8 = Rgba8::from_rgb(0x2E, 0xA0, 0x5A);

/// What a [`call_button`] is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CallTone {
    /// A switch that is on (the microphone, the camera): a frosted plate that
    /// reads over a picture as well as over the dark call screen.
    On,
    /// A switch that is *off* wears the solid cream plate: that is the state
    /// worth noticing, and the icon says which (`MIC_OFF`, `VIDEOCAM_OFF`).
    Off,
    /// Hang up, or decline: [`CALL_END_RED`].
    HangUp,
    /// Answer: [`CALL_ACCEPT_GREEN`].
    Accept,
}

/// The call screen's big buttons: a thumb's width, as on a phone's call screen.
pub const CALL_BUTTON_LARGE: f32 = 64.0;
/// The bar a minimised call or a ringing banner shows.
pub const CALL_BUTTON_SMALL: f32 = 36.0;

/// A round call control, `diameter` across.
pub fn call_button(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    tone: CallTone,
    diameter: f32,
) -> egui::Response {
    let size = egui::Vec2::splat(diameter);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = named(ui, resp, tooltip);
    let hovered = resp.hovered();
    let pressed = resp.is_pointer_button_down_on();
    let solid = |c: Color32| {
        let c = if pressed {
            c.gamma_multiply(0.85)
        } else if hovered {
            c.gamma_multiply(1.15)
        } else {
            c
        };
        // The glyph on a call colour is white in both themes, as on a phone.
        (c, c, DARK32.cream)
    };
    // The frosted plate of a switch that is on: white over the dark call
    // screen, black over a light banner.
    let veil = |a: u8| {
        if p32().dark {
            Color32::from_white_alpha(a)
        } else {
            Color32::from_black_alpha(a / 2)
        }
    };
    let (fill, ring, ink) = match tone {
        CallTone::HangUp => solid(CALL_END_RED.to_egui()),
        CallTone::Accept => solid(CALL_ACCEPT_GREEN.to_egui()),
        CallTone::Off => (
            if hovered {
                p32().accent_ring
            } else {
                p32().cream
            },
            p32().accent_ring,
            p32().ink,
        ),
        CallTone::On => (
            veil(if pressed {
                70
            } else if hovered {
                56
            } else {
                38
            }),
            veil(46),
            p32().cream,
        ),
    };
    paint_disc(
        ui.painter(),
        rect.center(),
        diameter * 0.5 - 1.0,
        Disc {
            fill,
            ring,
            ink,
            icon,
            icon_size: (diameter * 0.44).round(),
            ring_width: 1.0,
        },
    );
    paint_focus_ring(ui, &resp, rect, true);
    resp.on_hover_text(tooltip)
}

// The dock's menu disc used to be `mark_button` — [the mark](paint_mark) at
// [`round_button`]'s size, so the app's own logo was the button that opened the
// menu. It is an ordinary `round_button` carrying `icons::MENU` now: in a row
// where every other disc is a picture of what it does, the brand was the one
// that said nothing about its own contents. `paint_mark` keeps its other two
// callers (the boot splash and the sign-in card), which is where a logo belongs.

// ─── Depth ────────────────────────────────────────────────────────────────────
//
// One light, from above — the same scene the web UI draws in `depth.css`. A
// control standing on the map casts a soft shadow under it. (It also caught the
// light along its top, and an engaged one glowed, until the glass went
// on 2026-11-08; a flat plate under one shadow is the midas hub's look.)

/// The shadow a floating control casts: a little below it and soft, so it
/// reads as standing off the surface rather than printed on it.
pub(super) fn lift_shadow() -> Shadow {
    shadow([0, 3], 12, 100)
}

/// A soft black shadow at `alpha` in the dark theme, scaled by
/// [`Palette::shadow`]: on a light page the hub's lift is a faint one.
pub(super) fn shadow(offset: [i8; 2], blur: u8, alpha: u8) -> Shadow {
    Shadow {
        offset,
        blur,
        spread: 0,
        color: Color32::from_black_alpha((f32::from(alpha) * p32().shadow).round() as u8),
    }
}

/// The shadow under a card in a panel ([`list_group`], a chat row, a community
/// row): the midas hub's `--lift-2`, as near as one egui shadow comes — a few
/// points down, soft, darker than the page, so a card on a panel reads as
/// lying on it rather than printed into it.
#[must_use]
pub fn card_shadow() -> Shadow {
    shadow([0, 4], 14, 150)
}

/// The shadow under the chosen option of a [`segmented`] control and under a
/// button at rest: the hub's `--m-shadow`, close and small.
pub(super) fn segment_shadow() -> Shadow {
    shadow([0, 1], 4, 120)
}

/// The chosen option of a segmented control, painted: a raised card with the
/// small shadow ([`segmented`], the Community | World switch).
pub fn paint_raised(painter: &egui::Painter, rect: egui::Rect, corner: CornerRadius) {
    painter.add(segment_shadow().as_shape(rect, corner));
    painter.rect_filled(rect, corner, p32().surface_raised);
}

/// A shadow (or glow) under a disc. Painted before the disc itself.
pub(super) fn paint_disc_shadow(
    p: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    shadow: Shadow,
) {
    let rect = egui::Rect::from_center_size(center, egui::Vec2::splat(radius * 2.0));
    let corner = CornerRadius::same(radius.min(f32::from(u8::MAX)) as u8);
    p.add(shadow.as_shape(rect, corner));
}

pub(super) fn paint_disc(p: &egui::Painter, center: egui::Pos2, radius: f32, d: Disc<'_>) {
    // A flat plate under one soft shadow, as the midas hub draws its round
    // buttons. The sheen and the accent's glow went with the glass (2026-11-08).
    paint_disc_shadow(p, center, radius, lift_shadow());
    p.circle_filled(center, radius, d.fill);
    paint_contact_edge(p, center, radius + d.ring_width * 0.5);
    p.circle_stroke(center, radius, Stroke::new(d.ring_width, d.ring));
    p.text(
        center,
        egui::Align2::CENTER_CENTER,
        d.icon,
        FontId::proportional(d.icon_size),
        d.ink,
    );
}
