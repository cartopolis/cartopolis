use super::*;

// ─── Panel header ─────────────────────────────────────────────────────────────

/// Padding between the header band's edge and its contents.
pub(super) const HEADER_INSET: f32 = 7.0;

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
pub(super) fn header_height() -> f32 {
    44.0_f32.max(close_button_size() + 2.0 * HEADER_INSET)
}

/// A word that is an action beside a panel's title — Map layers' Reset: the
/// accent's text, no frame, a soft plate under the pointer. A small outlined
/// button there read as "unstyled" (2026-11-10).
pub fn header_action(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(label)
                .size(SIZE_BODY)
                .color(p().accent_word),
        )
        .frame_when_inactive(false),
    )
}

/// An icon action that sits *beside* something — the route panel's pick /
/// use-my-position / clear, the address bar's flag, a bookmark's rename and
/// delete. Compact under a mouse, finger-sized under a finger.
pub fn action_button(ui: &mut egui::Ui, label: impl Into<egui::WidgetText>) -> egui::Response {
    action_enabled(ui, true, label)
}

/// [`action_button`], for the ones that are conditionally live — "use my
/// position" without a fix, "update to here" on a place already at the camera.
pub fn action_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    // The web's small ghost button (`Button size=sm variant=ghost`), painted
    // by `controls::flat_button` — until 2026-11-22 an `egui::Button`, which
    // beside the painted controls read as egui's: a grey pill at rest, a
    // greyer one disabled. A one-glyph label in the icon font's private-use
    // block is the icon; anything else is the word.
    let label: egui::WidgetText = label.into();
    let text = label.text().to_string();
    let is_icon = {
        let mut chars = text.chars();
        matches!(
            (chars.next(), chars.next()),
            (Some(c), None) if ('\u{e000}'..='\u{f8ff}').contains(&c)
        )
    };
    let (icon, word) = if is_icon {
        (text.as_str(), "")
    } else {
        ("", text.as_str())
    };
    ui.add_enabled_ui(enabled, |ui| {
        super::controls::flat_button(
            ui,
            icon,
            word,
            super::control_height_small(),
            0.0,
            false,
            super::controls::Dress::Ghost,
        )
    })
    .inner
}

/// The width of a menu entry's icon column. Fixed, so the words line up down the
/// card whatever the glyphs are — a row of labels each starting where its own
/// icon happened to end is the thing that makes a list read as a pile.
pub(super) const MENU_ICON_COL: f32 = 26.0;

/// The height of one [`menu_entry`] row: a list row in the midas hub, not a
/// widget row. 36 pt on a pointer and 48 under a finger, since 2026-11-08 —
/// they were [`row_height`], 24 and 32, and "the menu items are too cramped"
/// was about these first.
#[must_use]
pub fn menu_row_height() -> f32 {
    if touch_layout() {
        48.0
    } else {
        36.0
    }
}

/// One line of the app menu: an icon, a word, and **no box**.
///
/// The menu was a stack of `ui.button`s in a justified layout, which egui draws
/// as full-width plates — a dozen of them, each looking like the primary action
/// of a dialog, with the four `selectable_label` entries between them drawing
/// nothing at all. So half the card looked like a form of buttons and half like
/// a list, and neither half said which entries were *on*.
///
/// A menu row is not a button: it is a line of a list you point at. It carries
/// its highlight on hover and its state in the accent, and that is the whole
/// visual vocabulary — the same one [`close_button`] uses for a frameless
/// control, one rung quieter than [`chip`].
///
/// Painted rather than assembled from `SelectableLabel` for the reason
/// [`round_button`] is painted: the icon column has to be a *column*, and a
/// label containing a glyph and two spaces is not one.
pub fn menu_entry(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    menu_entry_full(ui, icon, label, false, true, false)
}

/// [`menu_entry`] for a row that shows whether the thing it opens is open — the
/// panels, whose discs in the dock carry the same "this is the live one" accent.
pub fn menu_entry_selected(
    ui: &mut egui::Ui,
    selected: bool,
    icon: &str,
    label: &str,
) -> egui::Response {
    menu_entry_full(ui, icon, label, selected, true, false)
}

/// [`menu_entry`] for a row that is conditionally live — Home with no Home set,
/// Back at the start of the history.
pub fn menu_entry_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    icon: &str,
    label: &str,
) -> egui::Response {
    menu_entry_full(ui, icon, label, false, enabled, false)
}

/// [`menu_entry`] for a row that takes something away — remove, block,
/// delete: the error ink, as the web's `data-tone=danger` entry. The red was
/// a `RichText` colour on a raw `ui.button` until 2026-11-22, which
/// `menu_entry` could not carry.
pub fn menu_entry_danger(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    menu_entry_full(ui, icon, label, false, true, true)
}

pub(super) fn menu_entry_full(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    selected: bool,
    enabled: bool,
    danger: bool,
) -> egui::Response {
    // Full width whatever the layout: these live inside a fixed-width card, and a
    // row that sized to its own text would put the hover highlight on the words
    // rather than on the line.
    let height = menu_row_height().max(ui.spacing().interact_size.y);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, selected, label)
    });
    let hovered = enabled && resp.hovered();
    let ink = match (enabled, selected, hovered) {
        (false, _, _) => p().text_dim.gamma_multiply(0.6),
        _ if danger => p().error,
        (_, true, _) => p().accent_text,
        (_, false, true) => p().cream,
        _ => p().text,
    };
    let p = ui.painter();
    if hovered || resp.is_pointer_button_down_on() {
        let fill = if resp.is_pointer_button_down_on() {
            self::p().accent_muted
        } else {
            self::p().surface_hover
        };
        p.rect_filled(rect, CornerRadius::same(RADIUS_SM), fill);
    }
    if selected {
        // The same inset accent bar the sign-in card's rows wear: it is what says
        // "this one" without a box round it.
        let bar = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(1.0, 5.0),
            egui::vec2(3.0, height - 10.0),
        );
        p.rect_filled(bar, CornerRadius::same(2), self::p().accent);
    }
    let x = rect.left() + 8.0;
    if !icon.is_empty() {
        p.text(
            egui::pos2(x + MENU_ICON_COL * 0.5, rect.center().y),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(SIZE_VALUE),
            ink,
        );
    }
    p.text(
        egui::pos2(x + MENU_ICON_COL + 4.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(SIZE_VALUE),
        ink,
    );
    paint_focus_ring(ui, &resp, rect, false);
    resp
}

/// One segment of a switcher — `Auto`/`Day`/`Night`, `Auto`/`Manual`, the
/// clock's `Local`/`UTC`.
///
/// **A chip has to look like a control even when it is off.** These were
/// `ui.selectable_label`, whose unselected state paints no background and no
/// outline at all: the sun switcher rendered as the bare words "Auto Day Night"
/// with one of them in a coloured pill, so two of the three read as a caption for
/// the third rather than as the two other choices. Off is a plate now — the same
/// raised surface and hairline every button at rest wears — and on is the light
/// engaged plate the dock discs and [`map_disc`] wear for "this is the live one",
/// which is also what a segmented control looks like everywhere else: the
/// selected segment is the teal one.
///
/// It takes its height from the enclosing `interact_size` rather than pinning
/// one, which is what keeps it finger-sized inside a box that has already raised
/// that floor for the touch tier without making it a 32 pt slab on a desktop.
pub fn chip(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    ui.add(chip_button(label, selected))
}

/// [`chip`] at a stated size — for a segmented row whose segments have to be
/// equal width whatever their words are (the route sheet's travel modes).
///
/// Three chips sized to "Car", "Bike" and "Walk" read as three unrelated
/// buttons; one control with three positions has one width per position.
pub fn chip_sized(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    size: egui::Vec2,
) -> egui::Response {
    ui.add_sized(size, chip_button(label, selected))
}

pub(super) fn chip_button(label: &str, selected: bool) -> egui::Button<'static> {
    let (fill, stroke, ink) = if selected {
        (p().accent, p().accent_ring, p().on_accent)
    } else {
        (p().surface_raised, p().control_edge, p().text)
    };
    egui::Button::new(egui::RichText::new(label).size(SIZE_CHIP).color(ink))
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(RADIUS_SM))
}

/// The ✕ every panel closes with — **painted, not typed.**
///
/// `✕` is in none of the fonts egui ships, so every panel that
/// spelled its close button `ui.button("✕")` drew an empty box; the multiplication
/// sign that survives the probe is a comma-sized cross that reads as a typo at
/// button size. Two line segments have neither problem: they land on the pixel
/// grid at any scale and cannot depend on a font subset at all.
pub fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::Vec2::splat(close_button_size()), egui::Sense::click());
    let resp = named(ui, resp, "Close");
    // Frameless at rest: a header carries one obvious action, and boxing it makes
    // the title bar look like it has a second panel in it.
    let (fill, ring, ink) = if resp.is_pointer_button_down_on() {
        (p().accent_muted, p().accent_ring, p().cream)
    } else if resp.hovered() {
        (p().surface_hover, p().accent, p().cream)
    } else {
        (Color32::TRANSPARENT, Color32::TRANSPARENT, p().text_dim)
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
    paint_focus_ring(ui, &resp, rect, false);
    resp.on_hover_text("Close")
}

/// The ⋮ that opens a row's own actions — **painted, not typed**, and unlike
/// [`close_button`] it carries its frame at rest.
///
/// Painted for the reason the close button is, only more so: *every* vertical
/// ellipsis is missing from the bundled font. `⋮` (U+22EE), `︙` (U+FE19), `⁝`
/// (U+205D), `⠇` (braille) and even the horizontal `⋯` all fail
/// a probe of the loaded fonts, so there is no fallback chain to write here —
/// the survivors are `…` and `☰`, and neither of those is this control. Three
/// dots on the pixel grid have no font subset to depend on.
///
/// **Framed at rest is the whole point.** A place row's actions were reachable
/// only by long-pressing the row, and a long press advertises itself nowhere: the
/// hint was an `on_hover_text`, which a touchscreen never shows. The close button
/// can afford to be frameless because a panel header has one obvious action and a
/// box around it reads as a second panel; a list row is the opposite case, where
/// the affordance *is* the feature.
pub fn kebab_button(ui: &mut egui::Ui) -> egui::Response {
    let side = close_button_size();
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::click());
    let resp = named(ui, resp, "More");
    let (fill, ring, ink) = if resp.is_pointer_button_down_on() {
        (p().accent_muted, p().accent_ring, p().cream)
    } else if resp.hovered() {
        (p().surface_hover, p().accent, p().cream)
    } else {
        (p().surface_raised, p().control_edge, p().text_dim)
    };
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(RADIUS_SM),
        fill,
        Stroke::new(1.0, ring),
        egui::StrokeKind::Inside,
    );
    // Proportional to the target, for the reason the ✕'s inset is: the same
    // absolute dot on a 32 pt finger button and a 22 pt pointer one reads as two
    // different controls rather than as one at two sizes.
    let radius = (side * 0.055).max(1.0);
    let gap = side * 0.22;
    for step in [-1.0, 0.0, 1.0] {
        p.circle_filled(rect.center() + egui::vec2(0.0, step * gap), radius, ink);
    }
    paint_focus_ring(ui, &resp, rect, false);
    // Baked in rather than left to the caller for the reason `close_button`'s is:
    // this control's whole job is to advertise that a row has actions, and its
    // one caller supplying the words is not the same as the control carrying them.
    resp.on_hover_text("Actions")
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
pub fn panel_header(ui: &mut egui::Ui, icon: &str, title: &str) -> bool {
    panel_header_with(ui, icon, title, |_| {})
}

/// [`panel_header`] plus room for extra actions, laid out right-to-left from the
/// close button (the Places panel's "+ Folder").
pub fn panel_header_with(
    ui: &mut egui::Ui,
    icon: &str,
    title: &str,
    extra: impl FnOnce(&mut egui::Ui),
) -> bool {
    // **No band.** The header used to be a raised, bordered, shadowed box —
    // a panel inside the panel — which made every panel start with a second
    // frame and pushed the title down to body size. It is now the Chats
    // screen's title bar (`chats::list_screen`): a large title straight on the
    // surface and a hairline under it, so all the panels read as one family.
    let avail = ui.available_rect_before_wrap();
    let band = egui::Rect::from_min_size(avail.min, egui::vec2(avail.width(), header_height()));
    ui.painter().line_segment(
        [
            egui::pos2(band.left(), band.bottom() - 0.5),
            egui::pos2(band.right(), band.bottom() - 0.5),
        ],
        Stroke::new(1.0, p().overlay_line),
    );

    let close;
    {
        // The **actions are placed first**, from the right edge, and the title
        // gets what is left. The other way round is what the Places panel shipped
        // as: a truncating `Label` in a left-to-right layout claims the whole
        // remaining width (truncation is how it fits, so it always "fits"), which
        // left the right-to-left child rect empty and let "+ Folder"/"+ Place"
        // overflow leftwards *over* the panel's own name. Same bargain every row
        // in the Places panel makes with its kebab.
        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(band.shrink2(egui::vec2(0.0, HEADER_INSET)))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        row.spacing_mut().item_spacing.x = 8.0;
        close = close_button(&mut row).clicked();
        extra(&mut row);
        // A plain title, as the midas hub titles its pages: the teal medallion
        // that stood before it repeated the dock disc that opened the panel,
        // and was one more accent on a page that should carry one.
        let _ = icon;
        row.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(title)
                        .font(FontId::new(SIZE_PANEL_TITLE, brand_family()))
                        .color(p().cream),
                )
                .truncate()
                .selectable(false),
            );
        });
    }
    ui.advance_cursor_after_rect(band);
    ui.add_space(10.0);
    close
}
