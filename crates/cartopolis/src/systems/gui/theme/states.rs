use super::*;

// ─── The four states a designer forgets ───────────────────────────────────────
//
// This file owned colour, type, spacing, buttons, headers and checkboxes, and
// stopped there — it exported `ERROR` and `WARN` as *colours* and nothing that
// draws with them. So the app grew eight spinner sites in two idioms, seven
// error surfaces in seven constructions, five empty states in five shapes, and
// three mutually contradictory answers to "this control cannot act here" (hide
// it / grey it / leave it live and let the click do nothing).
//
// The touch consequence is the one that justifies the vocabulary on its own:
// **every disabled-state explanation in the client was an
// `on_disabled_hover_text`, which a touchscreen never shows.** `bookmarks`
// records that fact in a comment about long-press hints and then uses one
// anyway, to explain a button disabled during world load — i.e. on the app's
// primary platform, in the first thirty seconds a user ever spends in it, the
// entire disabled-state layer was addressed to nobody. That answer has to be
// fixable in one place, which means it has to *be* in one place.

/// A section title inside a panel.
///
/// Promoted out of `settings.rs`, which had the only named helper for this while
/// Appearance used a bare `ui.label`, About used `strong().color(CREAM)`, the
/// script editor used a `small()` dim label, and Layers overloaded a checkbox as
/// a heading. Four idioms across ten panels, and only one of them had a name.
///
/// Small capitals in the dim tone, as the contacts screen labels its groups
/// ("ONLINE NOW"): a section title is a signpost, and drawn bold in the brightest
/// tone it competed with the controls under it for the eye.
pub fn section(ui: &mut egui::Ui, title: &str) {
    section_tinted(ui, title, p32().text_dim);
}

/// [`section`] in a colour of its own: the admin pages' headings are
/// [`Palette::warn`] gold, the colour Admin wears everywhere it appears
/// (`docs/mockups/home-and-profile/v2`).
///
/// Drawn as the midas hub heads its groups ("Running 4", `.home__head`):
/// sentence case in the semibold face, and a trailing count — a title ending
/// in "(3)" — split off and set in the faint tone after it.
pub fn section_tinted(ui: &mut egui::Ui, title: &str, tone: Color32) {
    let (name, count) = split_count(title);
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            egui::RichText::new(sentence_case(name))
                .font(FontId::new(SIZE_SUB, brand_family()))
                .color(tone),
        );
        if let Some(n) = count {
            ui.label(
                egui::RichText::new(n)
                    .font(FontId::new(SIZE_SUB, brand_family()))
                    .color(p32().line),
            );
        }
    });
    ui.add_space(2.0);
}

/// "Asking to join (1)" → ("Asking to join", Some("1")).
pub(super) fn split_count(title: &str) -> (&str, Option<&str>) {
    if let Some(rest) = title.strip_suffix(')') {
        if let Some((name, n)) = rest.rsplit_once(" (") {
            if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                return (name, Some(n));
            }
        }
    }
    (title, None)
}

/// A title written in capitals by a caller ("ONLINE NOW") in sentence case;
/// anything already mixed-case is left as written, names and all.
pub(super) fn sentence_case(s: &str) -> String {
    if s.chars().any(char::is_lowercase) {
        return s.to_string();
    }
    let lower = s.to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// What a [`list_row_sized`] carries at its right-hand end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowEnd {
    /// Opens a page: a chevron.
    Chevron,
    /// Something waiting: the brand-orange pill before the chevron.
    Badge(usize),
    /// Nothing: an action row.
    Nothing,
}

/// A list of rows on one rounded surface, with hairlines between them: the
/// shape of the You page, Settings and Admin. One surface per group, so a page
/// reads as a few blocks of related things rather than one long column.
pub fn list_group<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let out = egui::Frame::NONE
        .fill(p32().card)
        .stroke(egui::Stroke::new(1.0, p32().list_line))
        .shadow(card_shadow())
        .corner_radius(egui::CornerRadius::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui)
        })
        .inner;
    ui.add_space(6.0);
    out
}

/// One row of a [`list_group`]: an icon on a small raised square, a title, an
/// optional line under it, and `end` on the right. `tone` is the icon's ink
/// ([`Palette::accent`] for most, [`Palette::warn`] on the admin pages,
/// [`Palette::error`] for a row that destroys something, whose title takes
/// the same colour). At a stated height, for a sheet that has to fit a phone
/// above the World | Community switch (your face's sheet, 44 pt rows: still
/// 66 dp, over [`MIN_TOUCH_TARGET_DP`]). `None` is the list's own 50 / 58.
pub fn list_row_sized(
    ui: &mut egui::Ui,
    height: Option<f32>,
    icon: &str,
    title: &str,
    sub: Option<&str>,
    end: RowEnd,
    tone: Color32,
) -> egui::Response {
    let h = height.unwrap_or(if sub.is_some() { 58.0 } else { 50.0 });
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let resp = named(ui, resp, title);
    let p = ui.painter();
    if resp.hovered() || resp.is_pointer_button_down_on() {
        p.rect_filled(rect, egui::CornerRadius::same(8), self::p32().surface_hover);
    }
    p.line_segment(
        [
            egui::pos2(rect.left() + 58.0, rect.bottom() - 0.5),
            egui::pos2(rect.right(), rect.bottom() - 0.5),
        ],
        egui::Stroke::new(1.0, self::p32().list_line),
    );
    let chip = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 12.0 + 16.0, rect.center().y),
        egui::Vec2::splat(32.0),
    );
    let chip_fill = if tone == self::p32().warn {
        self::p32().warn.gamma_multiply(0.14)
    } else {
        self::p32().surface_raised
    };
    p.rect_filled(chip, egui::CornerRadius::same(10), chip_fill);
    p.text(
        chip.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(17.0),
        tone,
    );

    // The right-hand end, painted first so the title knows how much room it has.
    let mut right = rect.right() - 12.0;
    let chevron = |p: &egui::Painter, right: &mut f32| {
        p.text(
            egui::pos2(*right, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            crate::systems::icons::CHEVRON_RIGHT,
            egui::FontId::proportional(18.0),
            self::p32().line,
        );
        *right -= 24.0;
    };
    match end {
        RowEnd::Chevron => chevron(p, &mut right),
        RowEnd::Badge(n) => {
            chevron(p, &mut right);
            if n > 0 {
                let g = p.layout_no_wrap(
                    if n > 99 { "99+".into() } else { n.to_string() },
                    egui::FontId::proportional(11.5),
                    Color32::WHITE,
                );
                let pill = egui::Rect::from_min_size(
                    egui::pos2(right - g.size().x - 14.0, rect.center().y - 9.5),
                    egui::vec2(g.size().x + 14.0, 19.0),
                );
                p.rect_filled(pill, egui::CornerRadius::same(10), BRAND);
                let gs = g.size();
                p.galley(pill.center() - gs / 2.0, g, Color32::WHITE);
                right = pill.left() - 8.0;
            }
        }
        RowEnd::Nothing => {}
    }

    let x = chip.right() + 12.0;
    let room = (right - x).max(20.0);
    let line = |text: &str, size: f32, color: Color32, y: f32| {
        let mut job = egui::text::LayoutJob::simple_singleline(
            text.to_string(),
            egui::FontId::proportional(size),
            color,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(room);
        let g = p.layout_job(job);
        let gy = y - g.size().y / 2.0;
        p.galley(egui::pos2(x, gy), g, color);
    };
    let title_tone = if tone == self::p32().error {
        self::p32().error
    } else {
        self::p32().text
    };
    match sub {
        Some(sub) => {
            line(title, 14.5, title_tone, rect.center().y - 9.0);
            line(sub, 12.0, self::p32().text_dim, rect.center().y + 10.0);
        }
        None => line(title, 14.5, title_tone, rect.center().y),
    }
    paint_focus_ring(ui, &resp, rect, false);
    resp
}

/// Where a page's back chevron says it was drawn, and where the dismissal
/// ladder leaves a press of Back or Escape for it: the pass of each.
pub(super) fn page_back_seen_id() -> egui::Id {
    egui::Id::new("theme::page_back_seen")
}
pub(super) fn page_back_pending_id() -> egui::Id {
    egui::Id::new("theme::page_back_pending")
}

/// A page's back chevron: whether it was `clicked`, or Back / Escape was
/// pressed for it.
///
/// Every chevron inside a panel goes through this, so the dismissal ladder
/// (`systems::dismiss`) steps back **one page** — Contacts to You, You to
/// Community — rather than closing the whole stack at once, which is what a
/// press did while the pages kept their own state out of the ladder's sight.
/// The page still decides where back goes; this only lets a key press it.
pub fn page_back(ctx: &egui::Context, clicked: bool) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(page_back_seen_id(), pass));
    let pressed = ctx
        .data_mut(|d| d.remove_temp::<u64>(page_back_pending_id()))
        // The ladder runs after the panel chain, so its press is read in the
        // next pass; anything older belonged to a page no longer drawn.
        .is_some_and(|at| pass.saturating_sub(at) <= 2);
    clicked || pressed
}

/// Whether a page with a back chevron was drawn in this pass or the last.
#[must_use]
pub fn page_back_shown(ctx: &egui::Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<u64>(page_back_seen_id()))
        .is_some_and(|at| pass.saturating_sub(at) <= 1)
}

/// Press the shown page's back chevron (the dismissal ladder's page rung).
pub fn press_page_back(ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        d.remove_temp::<u64>(page_back_seen_id());
        d.insert_temp(page_back_pending_id(), pass);
    });
}

/// One control with several positions: a raised pill with equal-width
/// segments, the chosen one on the [`Palette::accent`] plate in [`Palette::on_accent`] — the Chats
/// screen's toggle (`chats::segmented`) made general. Returns whether the
/// value changed; `value` is written only on a click, so a caller holding a
/// resource does not mark it changed every frame.
///
/// It replaced rows of `selectable_value`s, which wrapped as soon as a panel
/// was narrow ("Full detail" alone on a second line) and read as loose words
/// rather than one choice. Labels that do not fit their segment are set
/// smaller, down to [`SIZE_SUB`]; past that the options take two rows.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    if options.is_empty() {
        return false;
    }
    let gap = 2.0;
    let width = ui.available_width();
    let n = options.len();
    // How wide a label is, measured in the chosen option's (wider) brand face
    // so that choosing one never makes it spill.
    let label_w = |label: &str, size: f32| {
        ui.ctx().fonts_mut(|f| {
            f.layout_no_wrap(
                label.to_string(),
                FontId::new(size, brand_family()),
                p32().text,
            )
            .size()
            .x
        })
    };
    let seg_w_for = |cols: usize| (width - 6.0 - gap * (cols as f32 - 1.0)) / cols as f32;
    let fits = |cols: usize, size: f32| {
        let seg_w = seg_w_for(cols);
        options
            .iter()
            .all(|(_, label)| label_w(label, size) <= seg_w - 10.0)
    };
    // One row while every label fits it at [`SIZE_SUB`] or larger; otherwise
    // two rows, half the options in each, at the same sizes. Shrinking four
    // options into one row on a phone made them unreadable, and wrapping
    // inside a segment broke "Automatic" mid-word (2026-11-10).
    let cols = if fits(n, SIZE_SUB) { n } else { n.div_ceil(2) };
    let rows = n.div_ceil(cols);
    let seg_w = seg_w_for(cols);
    let mut size = SIZE_BODY - 1.0;
    while size > SIZE_TINY && !fits(cols, size) {
        size -= 0.5;
    }
    let row_h = row_height().max(30.0) - 6.0;
    let height = rows as f32 * row_h + (rows as f32 - 1.0) * gap + 6.0;
    let (rect, track) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let inner = rect.shrink(3.0);
    // Keyed on the track's own auto id: two rows with the same first label
    // ("Automatic") in one tab must not share segment ids.
    let base = track.id;

    // A card like the lists around it, the chosen option on the accent plate
    // (2026-11-10: the hub's deep well with a raised card on it was "ugly", of
    // the Clouds switch).
    let p = ui.painter().clone();
    p.rect_filled(rect, CornerRadius::same(RADIUS_LG), self::p32().card);
    p.rect_stroke(
        rect,
        CornerRadius::same(RADIUS_LG),
        Stroke::new(1.0, self::p32().list_line),
        egui::StrokeKind::Inside,
    );
    let mut changed = false;
    for (i, (option, label)) in options.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        let r = egui::Rect::from_min_size(
            egui::pos2(
                inner.left() + col as f32 * (seg_w + gap),
                inner.top() + row as f32 * (row_h + gap),
            ),
            egui::vec2(seg_w, row_h),
        );
        let resp = ui
            .interact(r, base.with(i), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let on = *value == *option;
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, *label)
        });
        let (fill, ink) = if on {
            (self::p32().accent, self::p32().on_accent)
        } else if resp.is_pointer_button_down_on() {
            (self::p32().surface_hover, self::p32().cream)
        } else if resp.hovered() {
            (self::p32().surface_raised, self::p32().cream)
        } else {
            (Color32::TRANSPARENT, self::p32().text_dim)
        };
        p.rect_filled(r, CornerRadius::same(RADIUS_LG - 3), fill);
        let font = FontId::new(
            size,
            if on {
                brand_family()
            } else {
                FontFamily::Proportional
            },
        );
        p.text(r.center(), egui::Align2::CENTER_CENTER, *label, font, ink);
        paint_focus_ring(ui, &resp, r, false);
        if resp.clicked() && !on {
            *value = *option;
            changed = true;
        }
    }
    changed
}

/// A line under a control stating a constraint or a stored intention — *not* a
/// failure. Plain dim text rather than a warning colour: it is a fact about the
/// build or the world, not something the user did wrong.
pub fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(SIZE_CHIP)
            .color(p32().text_dim)
            .italics(),
    );
}

/// An inline failure with a Dismiss, returning `true` when dismissed so the
/// caller clears its own `Option<String>`.
///
/// On a faint [`Palette::error`]-tinted chip rather than as bare coloured text, so that an
/// error can never again be styled as a footnote — the Search panel reported a
/// geocoding failure through `ui.weak()`, which is the *quietest* tone in the
/// palette, for the one string that most needs to be read.
#[must_use]
pub fn inline_error(ui: &mut egui::Ui, message: &str) -> bool {
    let mut dismissed = false;
    egui::Frame::NONE
        .fill(p32().error.gamma_multiply(0.16))
        .stroke(Stroke::new(1.0, p32().error.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(RADIUS_SM))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(message)
                            .size(SIZE_SUB)
                            .color(p32().error),
                    )
                    .wrap(),
                );
            });
            // One row's height, not `with_layout`: in a vertical parent that
            // takes every pixel left below it, and on the chat list the chip
            // swallowed the screen with Dismiss floating in the middle.
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    dismissed = action_button(ui, "Dismiss").clicked();
                },
            );
        });
    dismissed
}

/// How long an armed [`confirm_button`] waits before disarming itself.
pub(super) const CONFIRM_ARMED_S: f64 = 3.0;

/// A destructive action that takes two presses: the first arms the button and
/// swaps its label, the second does the thing. Returns `true` only on the second.
///
/// **Nothing in this client confirms anything.** Deleting a place removes two
/// JPEGs from `platform::storage`; "Remove the photo" sits one row under "View
/// the photo" with no separator, which on a phone is a ~20 pt mis-tap between
/// looking at a picture and destroying it; the script editor's Stop deletes a
/// script off the server; and there is no undo for any of them.
///
/// A modal would be the conventional answer and is the wrong shape here: this
/// app draws its panels into one shared root `Ui` with a hand-built ordering
/// chain, and a floating confirm dialog is a new `Area` fighting that chain for
/// a decision that is one bit wide. Arming in place costs no layer, cannot be
/// occluded, and puts the confirmation exactly where the finger already is.
///
/// State lives in `ui.memory` under `id`, so a caller stays a one-liner and no
/// panel needs a new field; the arm times out so a button left armed by a stray
/// tap does not lie in wait.
#[must_use]
pub fn confirm_button(ui: &mut egui::Ui, id: egui::Id, label: &str, confirm_label: &str) -> bool {
    let now = ui.input(|i| i.time);
    let armed_at: Option<f64> = ui.memory(|m| m.data.get_temp(id));
    let armed = armed_at.is_some_and(|t| now - t < CONFIRM_ARMED_S);

    // Armed, it is the web's danger button: the error tone's soft fill and
    // ink, the same height as the word it replaced.
    let resp = if armed {
        danger_button(ui, "", confirm_label)
    } else {
        action_button(ui, label)
    };

    if resp.clicked() {
        if armed {
            ui.memory_mut(|m| m.data.remove::<f64>(id));
            return true;
        }
        ui.memory_mut(|m| m.data.insert_temp(id, now));
        return false;
    }
    // Repaint while armed, or the label would sit on "Delete?" until some other
    // event happened to redraw the panel — a timeout nobody can see has not
    // timed out.
    if armed {
        ui.ctx().request_repaint();
    } else if armed_at.is_some() {
        ui.memory_mut(|m| m.data.remove::<f64>(id));
    }
    false
}

/// A checkbox whose box is square. egui derives the checkbox frame's rounding
/// from the shared widget visuals ([`RADIUS`], sized for buttons), and on a
/// ~14 pt box that radius reads as a circle — an unchecked toggle then looks
/// like a radio button, which promises "pick one" instead of "on/off". Scopes
/// the rounding down for just this widget; every other visual stays the
/// theme's. Use this, not `ui.checkbox`, everywhere.
pub fn checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    // A ticked box is a teal plate with a dark tick: "on" in the same
    // treatment as every other engaged control. egui strokes the tick in the
    // label's colour, so the box takes the accent through the style and the
    // tick is painted again over it in [`Palette::ink`].
    let was = *checked;
    ui.scope(|ui| {
        let square = CornerRadius::same(RADIUS_SM);
        let w = &mut ui.style_mut().visuals.widgets;
        for v in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
        ] {
            v.corner_radius = square;
            if was {
                v.bg_fill = p32().accent;
                v.bg_stroke = Stroke::new(1.0, p32().accent_ring);
            }
        }
        let resp = ui.checkbox(checked, text);
        if was && *checked {
            let size = ui.spacing().icon_width_inner;
            let c = egui::pos2(
                resp.rect.left() + ui.spacing().icon_width / 2.0,
                resp.rect.center().y,
            );
            let r = egui::Rect::from_center_size(c, egui::Vec2::splat(size));
            ui.painter().add(egui::Shape::line(
                vec![
                    egui::pos2(r.left(), r.center().y),
                    egui::pos2(r.center().x, r.bottom()),
                    egui::pos2(r.right(), r.top()),
                ],
                Stroke::new(2.0, p32().on_accent),
            ));
        }
        resp
    })
    .inner
}

/// A password field, with the eye that reveals what is in it.
///
/// **Why every password field in this client needs one.** A masked field is a
/// row of dots you cannot proof-read, which is a mild annoyance with a hardware
/// keyboard and a dead end with a thumb: the two platforms this client runs on
/// with no system IME type through an on-screen keyboard
/// ([`crate::systems::onscreen_keyboard`]) with no autocorrect and no key
/// preview, so a mistyped character is invisible, unlocatable, and answered by a
/// server that will only say the whole thing was wrong.
///
/// The reveal is **per field and per frame-run, never persisted**: it lives in
/// egui's temporary memory under the field's own id, so it is gone by the next
/// launch and a second password field on the same screen is not revealed by the
/// first.
///
/// The non-obvious part is the focus restore. Tapping the eye is a click that is
/// not on the field, and egui answers those by taking the field's focus away
/// (`SurrenderFocusOn::Clicks`) — which on a phone takes the on-screen keyboard
/// down with it, so revealing your password would cost you the keyboard you were
/// typing it with. Putting the focus back has to happen in the same pass, before
/// the keyboard's own system runs and reads `text_edit_focused()`; every UI
/// system runs before it, by construction (`gui::plugin`, the on-screen keyboard's `.after` edges).
pub fn password_field(ui: &mut egui::Ui, text: &mut String) -> egui::Response {
    let eye = close_button_size();
    ui.horizontal(|ui| {
        // The eye sits *inside* the field's right end, as the sign-in mockup
        // draws it: the field keeps the card's full width and the text stops
        // short of the eye (`text_field_inset`). Beside the field it took a
        // sixth of a 275 pt phone and left the password narrower than the
        // account name over it.
        let width = ui.available_width();
        // Keyed off this row's own `Ui`, not the field's auto-id: the field's id
        // is only known after it has been shown, and this has to be read before.
        let id = ui.id().with("password_shown");
        let shown = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
        let field = super::controls::text_field_inset(
            ui,
            egui::TextEdit::singleline(text)
                .password(!shown)
                .desired_width(width),
            eye + 6.0,
        );

        let rect = egui::Rect::from_min_size(
            egui::pos2(
                field.rect.right() - eye - 8.0,
                field.rect.center().y - eye / 2.0,
            ),
            egui::Vec2::splat(eye),
        );
        let resp = ui.interact(rect, field.id.with("eye"), egui::Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                ui.is_enabled(),
                shown,
                "Show the password",
            )
        });
        let ink = if resp.is_pointer_button_down_on() || shown {
            p32().accent
        } else {
            p32().text_dim
        };
        paint_eye(ui.painter(), rect, shown, ink);
        paint_focus_ring(ui, &resp, rect, false);
        if resp.clicked() {
            ui.data_mut(|d| d.insert_temp(id, !shown));
            // Only when it was ours to lose: tapping the eye of a field nobody is
            // typing into must not pull focus into it, or the on-screen keyboard
            // slides up over a card the user was only reading.
            if field.has_focus() || field.lost_focus() {
                crate::systems::onscreen_keyboard::restore_focus(ui.ctx(), field.id);
            }
        }
        let _ = resp.on_hover_text(if shown {
            "Hide the password"
        } else {
            "Show the password"
        });
        // The *field's* response, not a union with the eye's: callers test it for
        // `changed()` and `lost_focus()` to know whether the password was edited
        // or submitted, and a union would answer those for a look at it.
        field
    })
    .inner
}

/// An eye, painted rather than lettered.
///
/// The icon font ([`crate::systems::icons`]) is the right answer for a glyph and
/// is *not* available for this one: it is subset from a manifest, so
/// `visibility` / `visibility_off` are not in the shipped file, and putting them
/// there means re-running `tools/subset-icon-font.py` and committing a new LFS
/// binary for two characters. So it is drawn, on the same terms as
/// [`close_button`]'s cross and the on-screen keyboard's select-all band. Add the
/// icons and delete this the next time that font is re-cut.
///
/// Struck through when the password is *hidden*, which is the state the field
/// starts in — so the mark says what tapping it will do, not what is happening.
pub(super) fn paint_eye(p: &egui::Painter, rect: egui::Rect, shown: bool, ink: Color32) {
    let c = rect.center();
    let r = rect.width() * 0.30;
    let stroke = Stroke::new(1.4, ink);
    // The almond, as two arcs approximated by short segments — epaint has no
    // quadratic-curve primitive on the simple painter, and a circle would read as
    // a target rather than an eye.
    let arc = |up: f32| {
        let n = 10;
        let pts: Vec<egui::Pos2> = (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                let x = c.x + (t - 0.5) * 2.0 * r;
                // A parabola through (±r, 0) with its apex at 0.62 r.
                let y = c.y - up * 0.62 * r * (1.0 - (2.0 * t - 1.0).powi(2));
                egui::pos2(x, y)
            })
            .collect();
        pts
    };
    p.add(egui::Shape::line(arc(1.0), stroke));
    p.add(egui::Shape::line(arc(-1.0), stroke));
    p.circle_filled(c, r * 0.30, ink);
    if !shown {
        p.line_segment(
            [
                egui::pos2(c.x - r, c.y + r * 0.7),
                egui::pos2(c.x + r, c.y - r * 0.7),
            ],
            stroke,
        );
    }
}
