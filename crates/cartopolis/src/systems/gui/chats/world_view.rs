//! A chat's World view, still drawn by egui: the component library has no
//! edition of its header pill or its composer pill yet, so this part of the
//! messenger stays egui's until one is built.
//!
//! Everything else of the messenger is `chats`' state and the library's
//! screens (`lib_chats`).

use bevy::prelude::*;

use super::{
    apply, attach_entries, circle_button, display_name, kind_of, pick_attach, share_for,
    top_offset, Actions, Attach, ChatKind, ChatsView, SHARE_FOR, SHARE_NOTE,
};
use crate::resources::{OnlineSession, RightPanel};
use crate::systems::chat::ChatState;
use crate::systems::egui_manager::EguiMut;
use crate::systems::{icons, theme};
use crate::utils::clock::unix_ms;

/// Widest the World view's composer pill gets on a desktop window.
const WORLD_SHEET_MAX_WIDTH: f32 = 520.0;

/// Paint a row of text pieces centred in `rect`, `gap` apart.
fn centred_row(
    p: &egui::Painter,
    rect: egui::Rect,
    parts: &[(String, f32, egui::Color32)],
    gap: f32,
) {
    let galleys: Vec<_> = parts
        .iter()
        .map(|(t, size, c)| p.layout_no_wrap(t.clone(), egui::FontId::proportional(*size), *c))
        .collect();
    let total: f32 = galleys.iter().map(|g| g.size().x).sum::<f32>()
        + gap * galleys.len().saturating_sub(1) as f32;
    let mut x = rect.center().x - total / 2.0;
    for (g, (_, _, c)) in galleys.into_iter().zip(parts) {
        let y = rect.center().y - g.size().y / 2.0;
        let w = g.size().x;
        p.galley(egui::pos2(x, y), g, *c);
        x += w + gap;
    }
}

/// The "live" dot beside World (`.live`): brand orange with a soft halo.
fn live_dot(p: &egui::Painter, c: egui::Pos2) {
    p.circle_filled(
        c,
        6.5,
        egui::Color32::from_rgba_unmultiplied(0xFD, 0x60, 0x12, 64),
    );
    p.circle_filled(c, 3.5, theme::BRAND.to_egui());
}

/// The two sides of the header's switch: the chat (its thread, the panel)
/// and the World (the map).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Seg {
    Community,
    World,
}

/// The design's segmented toggle (`.seg`): a raised pill with a Chat side and
/// a World side, World carrying the live dot.
fn segmented(ui: &mut egui::Ui, left: &str, width: f32, on: Seg) -> Option<Seg> {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 40.0), egui::Sense::hover());
    let inner = rect.shrink(3.0);
    let segs = [Seg::Community, Seg::World];
    let n = segs.len() as f32;
    let w = (inner.width() - 2.0 * (n - 1.0)) / n;
    let id = ui.id().with("chats_segmented");

    let p = ui.painter().clone();
    // The midas hub's segmented control, as `theme::segmented`: a sunken track,
    // the side you are on a raised card with the strong ink.
    p.rect_filled(
        rect,
        egui::CornerRadius::same(theme::RADIUS),
        theme::p32().ink,
    );
    p.rect_stroke(
        rect,
        egui::CornerRadius::same(theme::RADIUS),
        egui::Stroke::new(1.0, theme::p32().surface_hover),
        egui::StrokeKind::Inside,
    );
    let corner = egui::CornerRadius::same(theme::RADIUS - 3);
    let size = 14.0;
    let mut picked = None;
    for (i, &seg) in segs.iter().enumerate() {
        let r = egui::Rect::from_min_size(
            egui::pos2(inner.left() + i as f32 * (w + 2.0), inner.top()),
            egui::vec2(w, inner.height()),
        );
        let resp = ui.interact(r, id.with(i), egui::Sense::click());
        let name = match seg {
            Seg::Community => left,
            Seg::World => "World",
        };
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), seg == on, name)
        });
        if resp.clicked() && seg != on {
            picked = Some(seg);
        }
        let selected = seg == on;
        if selected {
            theme::paint_raised(&p, r, corner);
        }
        let ink = if selected || resp.hovered() {
            theme::p32().cream
        } else {
            theme::p32().text_dim
        };
        match seg {
            Seg::Community => {
                let parts = vec![
                    (icons::CHAT.to_string(), 16.0, ink),
                    (left.to_string(), size, ink),
                ];
                centred_row(&p, r, &parts, 6.0);
            }
            Seg::World => {
                let label = p.layout_no_wrap("World".into(), egui::FontId::proportional(size), ink);
                let total = 7.0 + 6.0 + label.size().x;
                let x0 = r.center().x - total / 2.0;
                live_dot(&p, egui::pos2(x0 + 3.5, r.center().y));
                p.galley(
                    egui::pos2(x0 + 13.0, r.center().y - label.size().y / 2.0),
                    label,
                    ink,
                );
            }
        }
    }
    picked
}

/// The "+" menu: round buttons with a word under each, one icon for one
/// meaning (`docs/mockups/social-camera/`).
fn attach_menu(
    ui: &mut egui::Ui,
    view: &mut ChatsView,
    key: &str,
    kind: ChatKind,
    chat: &ChatState,
    act: &mut Actions,
    now: u64,
) {
    if kind != ChatKind::Private {
        theme::note(
            ui,
            "Places and locations go in private chats only: this one is public.",
        );
        return;
    }
    let sharing = chat.sharing.iter().any(|s| s.channel == key);
    let entries = attach_entries(chat, key);
    let cell = (ui.available_width() / 4.0).floor();
    let mut picked = None;
    for row in entries.chunks(4) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for &entry in row {
                ui.allocate_ui_with_layout(
                    egui::vec2(cell, theme::ROUND_BUTTON_SIZE + 22.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        let on = entry == Attach::Location && view.share_open;
                        if theme::round_button(ui, entry.icon(), entry.hint(), on).clicked() {
                            picked = Some(entry);
                        }
                        ui.label(
                            egui::RichText::new(entry.label())
                                .size(12.0)
                                .color(theme::p32().text),
                        );
                    },
                );
            }
        });
        ui.add_space(6.0);
    }
    if let Some(entry) = picked {
        pick_attach(entry, view, key, now, act);
    }
    if view.share_open && !sharing {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("for").color(theme::p32().text_dim));
            for (label, ms) in SHARE_FOR {
                if theme::action_button(ui, label).clicked() {
                    share_for(view, key, ms, act);
                }
            }
        });
        theme::note(ui, SHARE_NOTE);
    }
}

/// "+", the field and send: the design's composer, a round raised "+", a
/// raised pill of a field, and the pale round button that sends.
fn compose_row(ui: &mut egui::Ui, view: &mut ChatsView, key: &str, id: &str, act: &mut Actions) {
    let claim = std::mem::take(&mut view.claim_focus);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if circle_button(
            ui,
            if view.attach {
                icons::CLEAR
            } else {
                icons::ADD
            },
            false,
            true,
            "Send a place, or share where you are",
        )
        .clicked()
        {
            view.attach = !view.attach;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ready = !view.draft.trim().is_empty();
            let send = circle_button(ui, icons::SEND, true, ready, "Send");
            let field = egui::Frame::NONE
                .fill(theme::p32().surface_raised)
                .corner_radius(egui::CornerRadius::same(20))
                .inner_margin(egui::Margin::symmetric(14, 9))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // The field's location pin, at its right end: send
                        // what the camera is looking at. Private chats only —
                        // a point never goes into a public one.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ChatState::is_private(key) {
                                let pin = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(icons::PIN)
                                            .size(18.0)
                                            .color(theme::p32().text_dim),
                                    )
                                    .sense(egui::Sense::click()),
                                );
                                // The glyph is its only text: named for what it does.
                                if theme::named(ui, pin, "Send this view")
                                    .on_hover_text("Send this view")
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked()
                                {
                                    act.view_to = Some(key.to_string());
                                }
                            }
                            crate::systems::theme::text_field(
                                ui,
                                egui::TextEdit::singleline(&mut view.draft)
                                    .id(egui::Id::new(id))
                                    .frame(egui::Frame::NONE)
                                    .font(egui::FontId::proportional(14.5))
                                    .text_color(theme::p32().text)
                                    .desired_width(ui.available_width())
                                    .hint_text(
                                        egui::RichText::new("Message").color(theme::p32().line),
                                    ),
                            )
                        })
                        .inner
                    })
                    .inner
                })
                .inner;
            // The World sheet lifts this field above the on-screen keys, so
            // the keys belong on the bottom.
            crate::systems::onscreen_keyboard::field_makes_room(ui.ctx(), field.id);
            if claim {
                field.request_focus();
            }
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (send.clicked() || entered) && ready {
                act.send = Some(key.to_string());
            }
        });
    });
}

/// A chat's World view: one slim pill over the map (back, who, Chat | World),
/// and nothing else of the chat's own. The chat's points are pinned by
/// `place_pins`.
///
/// **It reuses the map's chat rather than drawing a second one** (2026-10-18,
/// `docs/mockups/social-camera/`). Its last lines are the map's transcript,
/// bottom-left, fading, narrowed to this chat (`chat_overlay`); writing is the
/// dock's chat button, whose composer pill is drawn here for this chat because
/// a private chat sends through the engine and not through a channel scope. The
/// dock, the joystick and the map buttons stay: the half-screen sheet this
/// replaced hid all of them, and stopped part-way down the screen.
#[allow(clippy::too_many_arguments)]
pub fn render_chat_world(
    mut egui: EguiMut,
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    mut overlay: ResMut<crate::systems::chat_overlay::ChatOverlay>,
    metrics: Res<crate::systems::ui_input::UiMetrics>,
) {
    let Some(key) = chat.world.clone() else {
        return;
    };
    if (online.is_none() && !view.demo) || driver.active || !right_panel.is(RightPanel::None) {
        return;
    }
    let Some(channel) = chat.channels.iter().find(|c| c.key == key) else {
        chat.world = None;
        return;
    };
    let kind = kind_of(&key);
    let name = display_name(&key, &channel.label);
    let n = channel.lines.len();
    view.mark_seen(&key, n);
    if view.open.as_deref() != Some(key.as_str()) {
        view.open_chat(&key);
    }

    let now = unix_ms();
    let viewport = egui.viewport_pts();
    let top = top_offset(&chat);
    let ctx = egui.ctx_mut().clone();
    let mut act = Actions::default();

    egui::Area::new(egui::Id::new("chat_world_header"))
        .order(egui::Order::Middle)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
        .show(&ctx, |ui| {
            theme::glass_frame()
                .corner_radius(egui::CornerRadius::same(24))
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let clicked =
                            circle_button(ui, icons::BACK, false, true, "Back to the chats")
                                .clicked();
                        if theme::page_back(ui.ctx(), clicked) {
                            act.back = true;
                        }
                        ui.label(
                            egui::RichText::new(&name)
                                .size(theme::SIZE_BODY)
                                .color(theme::p32().cream),
                        );
                        if segmented(ui, "Chat", 170.0, Seg::World) == Some(Seg::Community) {
                            act.to_world = false;
                            act.open = Some(key.clone());
                        }
                    });
                });
        });

    if overlay.composing {
        if overlay.take_claim() {
            view.claim_focus = true;
        }
        let keys = crate::systems::chat_overlay::keyboard_reserve(&metrics, true);
        let (bottom, _) = crate::systems::chat_overlay::stack_bottoms(true, keys, 0.0);
        let width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(WORLD_SHEET_MAX_WIDTH);
        egui::Area::new(egui::Id::new("chat_world_composer"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
            .show(&ctx, |ui| {
                ui.set_width(width);
                theme::glass_frame()
                    .corner_radius(egui::CornerRadius::same(26))
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_width(width - 12.0);
                        if view.attach {
                            attach_menu(ui, &mut view, &key, kind, &chat, &mut act, now);
                            ui.add_space(6.0);
                        }
                        compose_row(ui, &mut view, &key, "chat_world_field", &mut act);
                    });
            });
        // Sent, the pill goes, as the map's own does.
        if act.send.is_some() {
            overlay.close();
        }
    }

    // "Chat" in the header goes back to the thread, which is the panel.
    if act.open.is_some() {
        chat.world = None;
        *right_panel = RightPanel::Chat;
    }
    if act.back {
        chat.world = None;
        view.open = None;
        *right_panel = RightPanel::Chat;
        act.back = false;
    }
    apply(
        act,
        &mut chat,
        &mut view,
        online.as_deref(),
        &mut right_panel,
        &mut nav,
        now,
    );
}
