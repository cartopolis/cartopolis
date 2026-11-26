use super::*;

// ─── The form controls, as the web's ─────────────────────────────────────────
//
// cartopolis.org builds its controls from the hub's component library
// (`@garage44/mithril/components`: `Button` in four variants, `FieldText`,
// `FieldCheckbox`, `FieldSegmented` …), and the app is reviewed beside it. The
// controls here are those components drawn with egui's painter, one function
// per component, so a panel in the app and a page on the web are one design:
//
// | web (`m-*`)                         | here                                   |
// |-------------------------------------|----------------------------------------|
// | `Button variant=primary`            | [`primary_button_sized`]               |
// | `Button variant=secondary`          | [`secondary_button`]                   |
// | `Button variant=ghost`              | [`ghost_button`]                       |
// | `Button variant=danger`             | [`danger_button`]                      |
// | `Button iconOnly`                   | [`icon_button`]                        |
// | `Button size=sm`                    | the `_small` forms                     |
// | `.m-input` (`FieldText`, textarea)  | [`text_field`]                         |
// | `Field` (label · control · help)    | [`labelled`]                           |
// | `FieldCheckbox`                     | [`checkbox`]                           |
// | `FieldSegmented`                    | [`segmented`]                          |
// | `NavItem`, a list row               | [`list_row`], [`menu_entry`]           |
//
// The web's numbers: a control is 38 px tall (44 under a finger), radius 8,
// padding 16 (12 in a field), a 1 px border, a 3 px soft focus ring; a small
// control 30 (36). Here that is [`control_height`] and [`control_height_small`]
// on the same `touch_layout` switch the rest of the theme reads.
//
// Why painted and not `egui::Button` / egui's own `TextEdit` frame: egui has
// one `WidgetVisuals` per *state* for every widget type, so a text field, a
// checkbox and a button share one fill, one stroke and one radius, and a
// focused field took the pressed button's dark plate and `expansion` — the
// thick black rim the owner called "really badly styled" (2026-11-22). A frame
// drawn here can be a well where a button is a key.

/// The height of a control: the web's `--m-control-height`, 38 px on a
/// pointer and 44 under a finger, which here is the primary button's height
/// or the touch row, whichever is taller (40 / 40 today).
#[must_use]
pub fn control_height() -> f32 {
    PRIMARY_BUTTON_HEIGHT.max(row_height())
}

/// The small control: the web's `size=sm`, 30 px on a pointer. Under a
/// finger it is the touch row (40), not the web's 36: this app's floor is
/// [`MIN_TOUCH_TARGET_DP`] for anything a finger has to hit.
#[must_use]
pub fn control_height_small() -> f32 {
    if touch_layout() {
        row_height()
    } else {
        30.0
    }
}

/// How a [`ghost_button`] or [`icon_button`] is dressed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Dress {
    /// Transparent at rest, a soft plate under the pointer (the web's ghost).
    Ghost,
    /// A surface with a hairline (the web's secondary), for an icon alone.
    Outlined,
    /// The danger tone's soft fill and ink.
    Danger,
}

/// A button that is a word, or an icon and a word, with no frame at rest: the
/// web's `variant=ghost`, the most used button on cartopolis.org (97 of 280).
/// Under the pointer it gets a soft plate; `icon` may be empty.
pub fn ghost_button(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    flat_button(ui, icon, label, control_height(), 0.0, false, Dress::Ghost)
}

/// [`ghost_button`] holding a stated width, its word cut with an ellipsis to
/// fit: the name of a list row that is itself the row's action (a saved
/// place), where a button sized to a long name would push the row's other
/// controls off the panel.
pub fn ghost_button_wide(ui: &mut egui::Ui, icon: &str, label: &str, width: f32) -> egui::Response {
    flat_button(
        ui,
        icon,
        label,
        control_height(),
        width,
        false,
        Dress::Ghost,
    )
}

/// A destructive action: the web's `variant=danger`, the error tone's soft
/// fill with the error ink. Pair it with [`confirm_button`] for anything
/// without an undo.
pub fn danger_button(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    flat_button(ui, icon, label, control_height(), 0.0, false, Dress::Danger)
}

/// A square button of one icon at the small height, frameless: an icon action in a list
/// row (a bookmark's rename and delete), where four outlined squares beside a
/// name read as a toolbar.
pub fn icon_button_small(ui: &mut egui::Ui, icon: &str, name: &str) -> egui::Response {
    let resp = flat_button(
        ui,
        icon,
        "",
        control_height_small(),
        0.0,
        false,
        Dress::Ghost,
    );
    named(ui, resp, name).on_hover_text(name)
}

/// `min_width` 0 sizes the button to its word; wider, the word is cut with an
/// ellipsis to fit rather than widening the button past it (a list row's name).
pub(super) fn flat_button(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    height: f32,
    min_width: f32,
    drag: bool,
    dress: Dress,
) -> egui::Response {
    let small = height < control_height();
    let gap = 6.0;
    // The web's padding-inline: 16 at md, 12 at sm; an icon alone is square.
    let pad = if small { 12.0 } else { 16.0 };
    let icon_size = if small { 15.0 } else { 17.0 };
    let word_size = if small { SIZE_SUB } else { SIZE_BODY };
    let icon_g = (!icon.is_empty()).then(|| {
        ui.painter().layout_no_wrap(
            icon.to_string(),
            FontId::proportional(icon_size),
            p32().text,
        )
    });
    let i = icon_g.as_ref().map_or(0.0, |g| g.size().x);
    let word_g = (!label.is_empty()).then(|| {
        let mut job = egui::text::LayoutJob::simple_singleline(
            label.to_string(),
            FontId::new(word_size, brand_family()),
            p32().text,
        );
        if min_width > 0.0 {
            let room = min_width - 2.0 * pad - if i > 0.0 { i + gap } else { 0.0 };
            job.wrap = egui::text::TextWrapping::truncate_at_width(room.max(0.0));
        }
        ui.painter().layout_job(job)
    });
    let w = word_g.as_ref().map_or(0.0, |g| g.size().x);
    let content = i + w + if i > 0.0 && w > 0.0 { gap } else { 0.0 };
    let width = if word_g.is_some() {
        min_width.max(content + 2.0 * pad)
    } else {
        min_width.max(height)
    };
    // A row handle senses the drag itself: a drag-only widget laid over a
    // click widget is dragged on the press, and the click never lands
    // (`bookmarks` module docs).
    let sense = if drag {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::click()
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), sense);
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let enabled = ui.is_enabled();
    let held = resp.is_pointer_button_down_on();
    let hovered = resp.hovered();
    // Fill, hairline and ink by dress and state. The ghost is the web's: no
    // fill and no border at rest, `--m-hover` under the pointer. Outlined is
    // the secondary button's surface and hairline. Danger is the error tone's
    // soft fill (the tone at 16 %, as `inline_error` draws it) with the error
    // ink, firmer under the pointer.
    let soft_error = p32().error.gamma_multiply(0.16);
    let (fill, line, ink) = match (dress, held, hovered) {
        (Dress::Ghost, true, _) => (p32().surface_hover, Color32::TRANSPARENT, p32().cream),
        (Dress::Ghost, false, true) => (p32().surface_raised, p32().control_edge, p32().cream),
        (Dress::Ghost, false, false) => (Color32::TRANSPARENT, Color32::TRANSPARENT, p32().text),
        (Dress::Outlined, true, _) => (p32().surface_hover, p32().line, p32().cream),
        (Dress::Outlined, false, true) => (p32().surface_raised, p32().accent, p32().cream),
        (Dress::Outlined, false, false) => (p32().surface, p32().control_edge, p32().text),
        (Dress::Danger, true, _) => (p32().error.gamma_multiply(0.3), p32().error, p32().error),
        (Dress::Danger, false, true) => (
            p32().error.gamma_multiply(0.22),
            p32().error.gamma_multiply(0.6),
            p32().error,
        ),
        (Dress::Danger, false, false) => {
            (soft_error, p32().error.gamma_multiply(0.35), p32().error)
        }
    };
    let radius = CornerRadius::same(if small { 6 } else { RADIUS });
    let painter = ui.painter();
    if dress == Dress::Outlined && enabled && !held {
        painter.add(segment_shadow().as_shape(rect, radius));
    }
    painter.rect(
        rect,
        radius,
        fill,
        Stroke::new(1.0, line),
        egui::StrokeKind::Inside,
    );
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.5) };
    // Sized to its word, the word is centred; holding a stated width (a list
    // row's name), it starts at the left like the row's other text.
    let mut x = if min_width > 0.0 {
        rect.left() + pad
    } else {
        rect.center().x - content / 2.0
    };
    if let Some(g) = icon_g {
        let gw = g.size().x;
        painter.galley_with_override_text_color(
            egui::pos2(x, rect.center().y - g.size().y / 2.0),
            g,
            dim(if dress == Dress::Danger {
                p32().error
            } else {
                p32().accent
            }),
        );
        x += gw + gap;
    }
    if let Some(g) = word_g {
        let y = rect.center().y - g.size().y / 2.0;
        painter.galley_with_override_text_color(egui::pos2(x, y), g, dim(ink));
    }
    paint_focus_ring(ui, &resp, rect, false);
    resp
}

/// A text field drawn as the web's `.m-input`: a card-toned well with a
/// hairline, the control's height, 12 pt of side padding; the hairline
/// firms under the pointer, and with focus it takes the accent and a soft ring
/// outside it. Multiline and single-line alike — give it the `TextEdit` as
/// usual and leave its frame and margin to this.
///
/// Goes through `text_touch::field`, so a phone gets its selection handles.
/// The frame is handed to egui whole (`TextEdit::frame`), which egui then
/// paints as given: no pressed-button plate, no `expansion`.
pub fn text_field(ui: &mut egui::Ui, edit: egui::TextEdit<'_>) -> egui::Response {
    text_field_inset(ui, edit, 0.0)
}

/// [`text_field`] whose text stops `right_inset` short of the right edge, for
/// a control drawn inside the field there (the password's eye).
pub(super) fn text_field_inset(
    ui: &mut egui::Ui,
    edit: egui::TextEdit<'_>,
    right_inset: f32,
) -> egui::Response {
    let mut margin = Margin::symmetric(12, 9);
    margin.right = margin.right.saturating_add(right_inset.round() as i8);
    let frame = egui::Frame::new()
        .fill(p32().card)
        .stroke(Stroke::new(1.0, p32().control_edge))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(margin);
    let resp = crate::systems::text_touch::field(
        ui,
        edit.frame(frame)
            .min_size(egui::vec2(0.0, control_height())),
    );
    // The states are painted over the frame egui drew, since which state the
    // field is in is only known once it has been laid out.
    let rect = resp.rect;
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let radius = CornerRadius::same(RADIUS);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(RADIUS + 2),
            Stroke::new(3.0, p32().accent.gamma_multiply(0.3)),
            egui::StrokeKind::Inside,
        );
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, p32().accent),
            egui::StrokeKind::Inside,
        );
    } else if resp.hovered() && ui.is_enabled() {
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, p32().line),
            egui::StrokeKind::Inside,
        );
    }
    resp
}

/// A control with its label over it and a line of help under it: the web's
/// `Field`. `help` may be empty. The label is the small size, strong, in the
/// strong text tone; the help is the small size in the dim tone.
pub fn labelled<R>(
    ui: &mut egui::Ui,
    label: &str,
    help: &str,
    control: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(
            egui::RichText::new(label)
                .size(SIZE_SUB)
                .strong()
                .color(p32().cream),
        );
        let r = control(ui);
        if !help.is_empty() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(help)
                        .size(SIZE_SUB)
                        .color(p32().text_dim),
                )
                .wrap(),
            );
        }
        r
    })
    .inner
}

/// An on/off setting as the web's `FieldSwitch`: the words, and at the row's
/// end a 38×22 track with a round knob that slides to the accent when on. A
/// checkbox asks to be ticked; a switch states what is on, which is what a
/// setting is ("Stay signed in on this device").
pub fn switch(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let (track_w, track_h, knob) = (38.0, 22.0, 16.0);
    let height = control_height_small().max(track_h);
    let (rect, mut resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let is_on = *on;
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), is_on, label)
    });
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let enabled = ui.is_enabled();
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.5) };
    let painter = ui.painter();
    // The words give way to the track, cut with an ellipsis, never under it.
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.to_string(),
        FontId::proportional(SIZE_BODY),
        dim(p32().text),
    );
    job.wrap =
        egui::text::TextWrapping::truncate_at_width((rect.width() - track_w - 12.0).max(0.0));
    let words = painter.layout_job(job);
    painter.galley(
        egui::pos2(rect.left(), rect.center().y - words.size().y / 2.0),
        words,
        dim(p32().text),
    );
    let track = egui::Rect::from_center_size(
        egui::pos2(rect.right() - track_w / 2.0, rect.center().y),
        egui::vec2(track_w, track_h),
    );
    // The knob eases across in the state time every other toggle uses.
    let t = crate::systems::motion::toggle(
        ui.ctx(),
        resp.id.with("knob"),
        is_on,
        crate::systems::motion::STATE,
    );
    let (fill, line) = if is_on {
        (p32().accent, p32().accent_ring)
    } else {
        (p32().surface_hover, p32().control_edge)
    };
    painter.rect(
        track,
        CornerRadius::same(RADIUS_PILL),
        dim(fill),
        Stroke::new(1.0, dim(line)),
        egui::StrokeKind::Inside,
    );
    let x0 = track.left() + track_h / 2.0;
    let x1 = track.right() - track_h / 2.0;
    painter.circle_filled(
        egui::pos2(x0 + (x1 - x0) * t, track.center().y),
        knob / 2.0,
        dim(p32().knob_on),
    );
    paint_focus_ring(ui, &resp, track, false);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A hairline with a word in it — "or with" between the password form and
/// the other ways to sign in, as the sign-in mockup draws it
/// (`docs/mockups/redesign`, A2).
pub fn divider_with(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::proportional(SIZE_SUB),
        p32().text_dim,
    );
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), galley.size().y.max(16.0)),
        egui::Sense::hover(),
    );
    let gap = 10.0;
    let half = galley.size().x / 2.0 + gap;
    let y = rect.center().y;
    let line = Stroke::new(1.0, p32().overlay_line);
    let painter = ui.painter();
    painter.line_segment(
        [
            egui::pos2(rect.left(), y),
            egui::pos2(rect.center().x - half, y),
        ],
        line,
    );
    painter.line_segment(
        [
            egui::pos2(rect.center().x + half, y),
            egui::pos2(rect.right(), y),
        ],
        line,
    );
    painter.galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            y - galley.size().y / 2.0,
        ),
        galley,
        p32().text_dim,
    );
    ui.add_space(6.0);
}
