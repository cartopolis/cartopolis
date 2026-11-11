//! The map's chrome: one **World | Community** switch along the bottom, a
//! **search bar with your face** across the top, and the map's round buttons
//! in a column down the right (`docs/mockups/redesign`, v2, 2026-11-10).
//!
//! The app has two spaces. *World* is the map, with round buttons on it and
//! cards along the bottom; *Community* is the people — chats, groups, the
//! calendar — on a solid page (the `RightPanel::Chat` panel: the whole screen
//! on a phone, a side panel on a desktop). One switch moves between them, in
//! the same place in both, and your face sits top right in both: it opens the
//! sheet that holds everything about you and the app (`ui::render_main_menu`),
//! which replaced the ☰ menu.
//!
//! **What it replaced.** A dock of up to six labelled discs along the bottom
//! (Search, Say, the view mode, Camera, Saved, Menu), the layers and lens discs
//! in the top-left corner, and a Community | World switch at the top of the
//! map that only showed signed in. The person called the navigation not
//! intuitive (2026-11-08): nothing said how the world and the community hang
//! together, and the two were mixed in one row. Now:
//!
//!   - **Search** is the pill across the top, as every map app has it
//!     (`Ctrl+L` still opens it). It opens the Search panel; the field and
//!     the hits live there.
//!   - **Saved places** is a row of your face's sheet: it is yours, not the
//!     map's.
//!   - **Layers, the view mode, Locate and Camera** are a column on the right
//!     under the search bar ([`right_column`]); Locate is drawn by `hud`, at
//!     the slot this module gives it.
//!   - **The compass** stays top left, under the bar, with the lens and the
//!     light discs below it ([`left_slot`]), and **Say** — the nearby-chat
//!     composer, only in a session — under those: a world thing, kept on the
//!     map, on the left where the transcript it opens is.
//!
//! **The switch is always there**, signed out too: Community without an
//! account is the card that says what signing in is for (`systems::welcome`,
//! [`crate::systems::welcome::Welcome::community`]), never a dead end. On a
//! phone a panel is the whole screen and the map's chrome drops while one is
//! open (egui paints a floating `Area` over a panel, with no z-order that says
//! otherwise) — except the switch over Community, which is the way back.
//!
//! **In the panel chain**, at its tail, because it reads the map's rect: on a
//! desktop a side panel takes the right of the window, and the bar and the
//! switch sit over the map that is left, not under the panel.

use crate::systems::egui_manager::EguiMut;
use bevy::prelude::*;

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::theme;

/// Height of the World | Community switch: a 48 pt touch row.
const SWITCH_H: f32 = 48.0;
/// The switch's widest, on a desktop.
const SWITCH_MAX_W: f32 = 264.0;
/// Height of the search bar across the top.
const SEARCH_H: f32 = 44.0;
/// The search bar's widest, on a desktop. It is anchored at the map's top
/// right, so your face is at its end there too.
const SEARCH_MAX_W: f32 = 420.0;
/// Diameter of your face at the end of the search bar.
const FACE: f32 = 36.0;
/// Room under the view-mode disc for its word ("Fly", "Earth").
const MODE_WORD_H: f32 = 16.0;

/// How much of the bottom edge the switch claims, in points.
///
/// Exported so that anything else anchored down there — the cards, the
/// thumbstick, the scale bar, the composer — sits *above* it rather than
/// guessing. A second copy of this arithmetic is how two of them drift.
#[must_use]
pub fn reserved_bottom() -> f32 {
    theme::OSD_MARGIN + SWITCH_H
}

/// How much of the top edge the search bar claims: where both columns of map
/// buttons start.
#[must_use]
pub fn reserved_top() -> f32 {
    theme::OSD_MARGIN + SEARCH_H + theme::OSD_MARGIN
}

/// The top of the `i`th disc of the left column: the compass (0), the lens
/// (1), the light (2) and Say (3). One step is a disc and a gap, so the
/// column is one strip of evenly spaced controls for the view.
#[must_use]
pub fn left_slot(i: usize) -> f32 {
    reserved_top() + i as f32 * (theme::ROUND_BUTTON_SIZE + theme::OSD_GAP)
}

/// Where each disc of the right column goes, top down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RightColumn {
    pub layers: f32,
    pub mode: f32,
    /// `hud`'s locate disc, where there is an avatar to locate.
    pub locate: Option<f32>,
    pub camera: Option<f32>,
    /// The bottom of the last disc (and its words): what anything else on the
    /// right edge — the zoom rail, the floor picker — starts under.
    pub bottom: f32,
}

/// The right column under the search bar: Layers, the view mode (its word
/// under it), Locate (its caption beside it) and Camera — the mockup's four.
#[must_use]
pub fn right_column(has_locate: bool, has_camera: bool) -> RightColumn {
    let disc = theme::ROUND_BUTTON_SIZE;
    let layers = reserved_top();
    let mode = layers + disc + theme::OSD_GAP;
    let mut next = mode + disc + MODE_WORD_H + theme::OSD_GAP;
    let locate = has_locate.then(|| {
        let top = next;
        // Its caption ("±4 m", "Searching…") sits to the left of the disc
        // (`hud`), not under it: the room under it is what lets Follow's
        // touch controls and this column share a phone's right edge.
        next += disc + theme::OSD_GAP;
        top
    });
    let camera = has_camera.then(|| {
        let top = next;
        next += disc + theme::OSD_GAP;
        top
    });
    RightColumn {
        layers,
        mode,
        locate,
        camera,
        bottom: next - theme::OSD_GAP,
    }
}

/// What the chrome drew last frame, for what is drawn before it in the next:
/// the zoom rail and the floor picker start under the right column.
#[derive(Resource, Debug, Clone, Copy)]
pub struct MapChrome {
    pub right_bottom: f32,
}

impl Default for MapChrome {
    fn default() -> Self {
        Self {
            right_bottom: right_column(true, true).bottom,
        }
    }
}

/// The two spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    World,
    Community,
}

/// The people half of what the chrome reads, bundled under Bevy's
/// sixteen-parameter ceiling.
#[derive(bevy::ecs::system::SystemParam)]
pub struct People<'w> {
    chat: Res<'w, crate::systems::chat::ChatState>,
    view: ResMut<'w, crate::systems::chats::ChatsView>,
    identity: Res<'w, crate::resources::AuthIdentity>,
    contacts: Res<'w, crate::systems::contacts::Contacts>,
    welcome: ResMut<'w, crate::systems::welcome::Welcome>,
    bridges: ResMut<'w, crate::systems::bridges::Bridges>,
}

/// What owns the screen instead of the map's chrome, bundled for the same
/// reason.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Covers<'w> {
    driver: Res<'w, crate::systems::driver::DriverState>,
    keyboard: Res<'w, crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    walk: Res<'w, crate::systems::walk_guide::WalkGuide>,
}

/// Draw the switch, the search bar with your face, the right column and the
/// lens and Say discs, and act on them.
#[allow(clippy::too_many_arguments)]
pub fn render_dock(
    mut egui: EguiMut,
    mut right_panel: ResMut<RightPanel>,
    mut menu: ResMut<crate::systems::ui::MainMenu>,
    mut camera_state: ResMut<crate::resources::CameraState>,
    session: Res<crate::systems::session::SessionState>,
    online: Option<Res<OnlineSession>>,
    covers: Covers,
    mut say: ResMut<crate::systems::chat_overlay::ChatOverlay>,
    avatars: Query<(), With<crate::components::Avatar>>,
    mut camera: ResMut<crate::systems::camera_screen::CameraScreen>,
    mut lens: ResMut<crate::systems::scenario::ScenarioView>,
    data_layers: Res<crate::systems::data_layers::DataLayers>,
    mut people: People,
    mut chrome: ResMut<MapChrome>,
) {
    let has_camera = crate::systems::camera_screen::available();
    let has_avatar = !avatars.is_empty();
    chrome.right_bottom = right_column(has_avatar, has_camera).bottom;
    // Driving owns the screen, and so does the camera.
    if covers.driver.active || camera.is_open() {
        crate::systems::place_pins::set_chrome_holes(egui.ctx(), Vec::new());
        menu.open = false;
        return;
    }
    let has_session = online.is_some();
    let demo = people.view.demo;
    let signed_in = has_session || demo;
    // A panel other than Community opening ends the signed-out Community card:
    // the person went somewhere else.
    if !right_panel.is(RightPanel::None) && !right_panel.is(RightPanel::Chat) {
        people.welcome.community = false;
    }
    let community_open = right_panel.is(RightPanel::Chat) && signed_in;
    let space = if community_open || people.welcome.community {
        Space::Community
    } else {
        Space::World
    };
    let compact = egui.compact();
    let viewport = egui.viewport_pts();
    // The map: what the side panels left of the root `Ui`. On a phone with a
    // panel up that is nothing, and the switch centres on the screen.
    let map = egui.ui_mut().available_rect_before_wrap();
    let panel_open = !right_panel.is(RightPanel::None);
    let covered = compact && panel_open;
    let keyboard = covers.keyboard.is_open();
    let ctx = egui.ctx_mut().clone();

    let me = people
        .identity
        .username
        .clone()
        .filter(|n| !n.is_empty())
        .or_else(|| demo.then(|| "jeroen".to_string()));
    let me = me.filter(|_| signed_in);
    let waiting = signed_in
        && (!people.contacts.incoming.is_empty() || people.view.admin.is_some_and(|n| n > 0));

    // ── The switch ───────────────────────────────────────────────────────
    // Over Community on a phone too — it is the way back — but not inside a
    // chat, whose composer has the bottom edge, nor over the keys.
    let switch_shown = !keyboard && (!covered || (community_open && people.view.open.is_none()));
    let mut picked = None;
    if switch_shown {
        let (left, right) = if covered {
            (0.0, viewport.x)
        } else {
            (map.left(), map.right())
        };
        let width = (right - left - 2.0 * theme::OSD_MARGIN).min(SWITCH_MAX_W);
        let centre = (left + right) / 2.0;
        let unread = if signed_in {
            people.view.unread_total(&people.chat)
        } else {
            0
        };
        egui::Area::new(egui::Id::new("space_switch"))
            .anchor(
                egui::Align2::CENTER_BOTTOM,
                egui::vec2(centre - viewport.x / 2.0, -theme::OSD_MARGIN),
            )
            .show(&ctx, |ui| {
                picked = space_switch(ui, width, space, unread);
            });
    }

    // ── The search bar and your face ─────────────────────────────────────
    // Not over a full-screen panel, nor a chat's World view, whose header has
    // the top of the map, nor while walking a route, whose next turn has it.
    let bar_shown = !covered && !keyboard && people.chat.world.is_none() && !covers.walk.active;
    let mut search_clicked = false;
    let mut face_clicked = false;
    if bar_shown {
        let width = (map.width() - 2.0 * theme::OSD_MARGIN).min(SEARCH_MAX_W);
        // On a desktop with Community open the panel wears your face at its
        // own top right; one face, not two.
        let with_face = !community_open;
        egui::Area::new(egui::Id::new("search_bar"))
            .anchor(
                egui::Align2::RIGHT_TOP,
                egui::vec2(
                    map.right() - viewport.x - theme::OSD_MARGIN,
                    theme::OSD_MARGIN,
                ),
            )
            .show(&ctx, |ui| {
                // After Show in world, the way back stands where the search
                // field was, with your face still at its end (bridge ②, N7).
                if let Some(back) = people.bridges.back.as_ref().map(|b| b.label.clone()) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let chip_w = if with_face { width - FACE - 8.0 } else { width };
                        if crate::systems::bridges::back_chip(ui, chip_w, SEARCH_H, &back) {
                            people.bridges.back_pressed = true;
                        }
                        if with_face {
                            let resp = face_button(ui, SEARCH_H, me.as_deref(), waiting, menu.open);
                            face_clicked = resp.clicked();
                            menu.face = Some(resp.rect);
                        }
                    });
                    return;
                }
                let out = search_bar(
                    ui,
                    width,
                    right_panel.is(RightPanel::Search),
                    with_face.then_some(Face {
                        me: me.as_deref(),
                        waiting,
                        open: menu.open,
                    }),
                );
                search_clicked = out.search;
                face_clicked = out.face;
                if let Some(rect) = out.face_rect {
                    menu.face = Some(rect);
                }
            });
    }

    // ── The right column, and the lens and Say on the left ───────────────
    // Dropped while any panel is open, as the compass and the touch controls
    // are: on a desktop the panel docks to the right edge they stand on.
    let mut clicked: Option<RightPanel> = None;
    let mut camera_clicked = false;
    let mut say_clicked = false;
    let mut holes = Vec::new();
    if !panel_open && !keyboard {
        let col = right_column(has_avatar, has_camera);
        // What the map's overlays keep off (`place_pins::clear_of_chrome`):
        // both columns, edge to edge of their discs and words.
        let disc = theme::ROUND_BUTTON_SIZE;
        holes.push(egui::Rect::from_min_max(
            egui::pos2(viewport.x - theme::OSD_MARGIN - disc, col.layers),
            egui::pos2(viewport.x - theme::OSD_MARGIN, col.bottom),
        ));
        holes.push(egui::Rect::from_min_max(
            egui::pos2(theme::OSD_MARGIN, left_slot(0)),
            egui::pos2(
                theme::OSD_MARGIN + disc,
                left_slot(if has_session { 3 } else { 2 }) + disc,
            ),
        ));
        let disc = |id: &str, top: f32| {
            egui::Area::new(egui::Id::new(id))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-theme::OSD_MARGIN, top))
        };
        disc("layers_disc", col.layers).show(&ctx, |ui| {
            if theme::round_button(
                ui,
                RightPanel::Layers.icon(),
                RightPanel::Layers.label(),
                false,
            )
            .clicked()
            {
                clicked = Some(RightPanel::Layers);
            }
        });
        disc("mode_disc", col.mode).show(&ctx, |ui| {
            mode_disc(ui, &mut camera_state, &session, has_avatar);
        });
        if let Some(top) = col.camera {
            disc("camera_disc", top).show(&ctx, |ui| {
                // The camera, dressed like every other disc: it wore the brand
                // orange from 2026-10-18 and read as switched on when nothing
                // was (2026-11-03).
                camera_clicked = theme::round_button(
                    ui,
                    crate::systems::icons::CAMERA,
                    "Camera",
                    camera.is_open(),
                )
                .clicked();
            });
        }

        // The lens (`systems::scenario`): figures you read, one at a time,
        // apart from the switches Layers holds. Under the compass, a control
        // of the view as it is.
        let has_lens = data_layers.scenario.is_some();
        let left = |id: &str, slot: usize| {
            egui::Area::new(egui::Id::new(id)).anchor(
                egui::Align2::LEFT_TOP,
                egui::vec2(theme::OSD_MARGIN, left_slot(slot)),
            )
        };
        left("lens_disc", 1).show(&ctx, |ui| {
            let on = crate::systems::scenario::lens_disc_on(&lens, has_lens);
            if theme::round_button(
                ui,
                crate::systems::icons::POLL,
                "Lens: figures about places",
                on,
            )
            .clicked()
            {
                crate::systems::scenario::press_lens_disc(&mut lens, has_lens);
                menu.open = false;
            }
        });
        // Say — the composer over the map (`systems::chat_overlay`), only in
        // a session. It carries the accent while the composer is up.
        if has_session {
            left("say_disc", 3).show(&ctx, |ui| {
                say_clicked = theme::round_button(
                    ui,
                    crate::systems::icons::CHAT,
                    "Say something to the people near you",
                    say.composing,
                )
                .clicked();
            });
        }
    }

    crate::systems::place_pins::set_chrome_holes(&ctx, holes);

    // ── Acting on it ─────────────────────────────────────────────────────
    match picked {
        Some(Space::World) => {
            // World is the map with no panel: a tap from anywhere comes back
            // to it, a side panel included.
            *right_panel = RightPanel::None;
            people.welcome.community = false;
            menu.open = false;
        }
        Some(Space::Community) => {
            if signed_in {
                *right_panel = RightPanel::Chat;
                people.view.open = None;
                people.view.you = false;
            } else {
                // Signed out, Community is what signing in is for, with the
                // way in and a way back to the map (`systems::welcome`).
                *right_panel = RightPanel::None;
                people.welcome.community = true;
            }
            menu.open = false;
        }
        None => {}
    }
    if search_clicked {
        right_panel.toggle(RightPanel::Search);
        menu.open = false;
    }
    if face_clicked {
        menu.open = !menu.open;
    }
    if let Some(panel) = clicked {
        right_panel.toggle(panel);
        menu.open = false;
    }
    if camera_clicked {
        camera.request = Some(crate::systems::camera_screen::Target::Anywhere);
        menu.open = false;
    }
    if say_clicked {
        if say.composing {
            say.close();
        } else {
            say.open();
        }
        menu.open = false;
    }
}

/// The World | Community switch: a pill track, the chosen side the accent
/// plate with white ink, an unread count as a gold badge on Community.
/// Answers the side tapped, if it was not the one already chosen — or World
/// tapped again, which is how a side panel over the map is put away.
fn space_switch(ui: &mut egui::Ui, width: f32, on: Space, unread: usize) -> Option<Space> {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, SWITCH_H), egui::Sense::hover());
    let pill = egui::CornerRadius::same(theme::RADIUS_PILL);
    let p = ui.painter().clone();
    p.add(theme::card_shadow().as_shape(rect, pill));
    p.rect(
        rect,
        pill,
        theme::p().map_fill,
        egui::Stroke::new(1.0, theme::p().map_rim),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(4.0);
    let gap = 2.0;
    let seg_w = (inner.width() - gap) / 2.0;
    let mut picked = None;
    for (i, (side, icon, label)) in [
        (Space::World, crate::systems::icons::CAT_MAP, "World"),
        (Space::Community, crate::systems::icons::GROUP, "Community"),
    ]
    .into_iter()
    .enumerate()
    {
        let r = egui::Rect::from_min_size(
            egui::pos2(inner.left() + i as f32 * (seg_w + gap), inner.top()),
            egui::vec2(seg_w, inner.height()),
        );
        let resp = ui
            .interact(r, ui.id().with(("space_switch", i)), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let chosen = side == on;
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, true, chosen, label)
        });
        if resp.clicked() && (!chosen || side == Space::World) {
            picked = Some(side);
        }
        let (fill, ink) = if chosen {
            (theme::p().accent, theme::p().on_accent)
        } else if resp.is_pointer_button_down_on() {
            (theme::p().accent_muted, theme::p().cream)
        } else if resp.hovered() {
            (theme::p().surface_hover, theme::p().cream)
        } else {
            (egui::Color32::TRANSPARENT, theme::p().map_ink)
        };
        p.rect_filled(r, pill, fill);
        let badge = (side == Space::Community && unread > 0).then(|| {
            if unread > 99 {
                "99+".to_string()
            } else {
                unread.to_string()
            }
        });
        paint_segment(&p, r, icon, label, chosen, ink, badge.as_deref());
    }
    picked
}

/// One side of the switch: its icon, its word and its badge, centred, set
/// smaller and then without the icon before anything would spill.
fn paint_segment(
    p: &egui::Painter,
    r: egui::Rect,
    icon: &str,
    label: &str,
    chosen: bool,
    ink: egui::Color32,
    badge: Option<&str>,
) {
    let family = if chosen {
        theme::brand_family()
    } else {
        egui::FontFamily::Proportional
    };
    let badge_g = badge.map(|b| {
        p.layout_no_wrap(
            b.to_string(),
            egui::FontId::proportional(11.0),
            theme::p().on_gold,
        )
    });
    let badge_w = badge_g
        .as_ref()
        .map_or(0.0, |g| g.size().x.max(8.0) + 10.0 + 5.0);
    let room = r.width() - 12.0;
    let mut size = theme::SIZE_BODY + 0.5;
    let mut with_icon = true;
    let fits = |size: f32, with_icon: bool| {
        let w = p
            .layout_no_wrap(
                label.to_string(),
                egui::FontId::new(size, family.clone()),
                ink,
            )
            .size()
            .x;
        w + badge_w + if with_icon { 18.0 + 6.0 } else { 0.0 } <= room
    };
    while !fits(size, with_icon) {
        if size > theme::SIZE_SUB {
            size -= 0.5;
        } else if with_icon {
            with_icon = false;
            size = theme::SIZE_BODY;
        } else {
            break;
        }
    }
    let word = p.layout_no_wrap(label.to_string(), egui::FontId::new(size, family), ink);
    let total = word.size().x + badge_w + if with_icon { 24.0 } else { 0.0 };
    let mut x = r.center().x - total / 2.0;
    let y = r.center().y;
    if with_icon {
        p.text(
            egui::pos2(x + 9.0, y),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(18.0),
            ink,
        );
        x += 24.0;
    }
    let wh = word.size();
    p.galley(egui::pos2(x, y - wh.y / 2.0), word, ink);
    x += wh.x + 5.0;
    if let Some(g) = badge_g {
        let w = g.size().x.max(8.0) + 10.0;
        let pill = egui::Rect::from_min_size(egui::pos2(x, y - 9.0), egui::vec2(w, 18.0));
        p.rect_filled(pill, egui::CornerRadius::same(9), theme::p().gold);
        let gs = g.size();
        p.galley(pill.center() - gs / 2.0, g, theme::p().on_gold);
    }
}

/// Who the face at the end of the search bar is.
struct Face<'a> {
    /// Your name, signed in; `None` draws the grey person.
    me: Option<&'a str>,
    /// A message request or a report waits: an orange dot on the face.
    waiting: bool,
    /// The sheet the face opens is open.
    open: bool,
}

#[derive(Default)]
struct BarOut {
    search: bool,
    face: bool,
    face_rect: Option<egui::Rect>,
}

/// The search pill: a magnifier and "Search places", and your face at its end.
fn search_bar(ui: &mut egui::Ui, width: f32, open: bool, face: Option<Face<'_>>) -> BarOut {
    let mut out = BarOut::default();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, SEARCH_H), egui::Sense::hover());
    let pill = egui::CornerRadius::same(theme::RADIUS_PILL);
    let face_rect = face.as_ref().map(|_| {
        egui::Rect::from_center_size(
            egui::pos2(rect.right() - 4.0 - FACE / 2.0, rect.center().y),
            egui::Vec2::splat(FACE),
        )
    });
    let field = egui::Rect::from_min_max(
        rect.min,
        egui::pos2(
            face_rect.map_or(rect.right(), |f| f.left() - 2.0),
            rect.bottom(),
        ),
    );
    let resp = ui.interact(
        field,
        ui.id().with("search_bar_field"),
        egui::Sense::click(),
    );
    let resp = theme::named(ui, resp, "Search places");
    let p = ui.painter().clone();
    p.add(theme::card_shadow().as_shape(rect, pill));
    let ring = if open || resp.hovered() {
        theme::p().accent
    } else {
        theme::p().map_rim
    };
    p.rect(
        rect,
        pill,
        theme::p().map_fill,
        egui::Stroke::new(if open { 1.5 } else { 1.0 }, ring),
        egui::StrokeKind::Inside,
    );
    let y = rect.center().y;
    p.text(
        egui::pos2(rect.left() + 16.0 + 9.0, y),
        egui::Align2::CENTER_CENTER,
        crate::systems::icons::SEARCH,
        egui::FontId::proportional(19.0),
        theme::p().map_ink,
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        "Search places".to_string(),
        egui::FontId::proportional(theme::SIZE_VALUE),
        theme::p().text_dim,
    );
    job.wrap =
        egui::text::TextWrapping::truncate_at_width((field.right() - rect.left() - 52.0).max(10.0));
    let g = p.layout_job(job);
    let gh = g.size().y;
    p.galley(
        egui::pos2(rect.left() + 44.0, y - gh / 2.0),
        g,
        theme::p().text_dim,
    );
    out.search = resp.on_hover_text("Search places (Ctrl+L)").clicked();

    if let (Some(face), Some(fr)) = (face, face_rect) {
        let resp = ui.interact(fr, ui.id().with("search_bar_face"), egui::Sense::click());
        let resp = theme::named(ui, resp, "You");
        paint_face(
            &p,
            fr.center(),
            FACE,
            face.me,
            face.waiting,
            face.open || resp.hovered(),
        );
        out.face = resp
            .on_hover_text("You: your profile, settings and more")
            .clicked();
        out.face_rect = Some(fr);
    }
    out
}

/// Your face, `size` across: your picture or your initial signed in, a grey
/// person signed out; `lit` rings it in the accent (its sheet is open, or the
/// pointer is on it), and `waiting` puts an orange dot on it.
pub(crate) fn paint_face(
    p: &egui::Painter,
    c: egui::Pos2,
    size: f32,
    me: Option<&str>,
    waiting: bool,
    lit: bool,
) {
    let r = size / 2.0;
    match me {
        Some(name) => crate::systems::chats::paint_avatar(
            p,
            c,
            name,
            crate::systems::chats::ChatKind::Private,
            size,
        ),
        None => {
            p.circle_filled(c, r, theme::p().surface_hover);
            p.text(
                c,
                egui::Align2::CENTER_CENTER,
                crate::systems::icons::APPEARANCE,
                egui::FontId::proportional(size * 0.55),
                theme::p().text_dim,
            );
        }
    }
    if lit {
        p.circle_stroke(c, r + 1.5, egui::Stroke::new(2.0, theme::p().accent));
    }
    if waiting {
        let dot = c + egui::vec2(r * 0.72, -r * 0.72);
        p.circle_filled(dot, 6.0, theme::p().surface);
        p.circle_filled(dot, 4.5, theme::BRAND);
    }
}

/// Your face as a control of its own: the Community page's top right.
/// Answers its response; the caller opens the sheet and records the rect.
pub(crate) fn face_button(
    ui: &mut egui::Ui,
    size: f32,
    me: Option<&str>,
    waiting: bool,
    open: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click());
    let resp = theme::named(ui, resp, "You");
    paint_face(
        ui.painter(),
        rect.center(),
        size,
        me,
        waiting,
        open || resp.hovered(),
    );
    resp.on_hover_text("You: your profile, settings and more")
}

/// A word under a disc on the map: light, with a narrow shadow under it, as a
/// map draws its own labels (a full halo smudged over a busy city, a plate
/// round it was "only uglier", both 2026-11-10).
fn word_under(painter: &egui::Painter, at: egui::Pos2, word: &str) {
    // In the buttons' own ink, edged in their plate: teal on white in the
    // light theme, light on the dark plate in the dark one. The cream word on
    // a black shadow (2026-11-10, the dock's) all but vanished over a sunlit
    // city in the light theme, at 10.5 pt.
    let font = egui::FontId::new(theme::SIZE_SUB, theme::brand_family());
    let edge = theme::p().map_fill;
    for (dx, dy) in [
        (-1.0, 0.0),
        (1.0, 0.0),
        (0.0, -1.0),
        (0.0, 1.0),
        (-0.7, -0.7),
        (0.7, -0.7),
        (-0.7, 0.7),
        (0.7, 0.7),
    ] {
        painter.text(
            at + egui::vec2(dx, dy),
            egui::Align2::CENTER_CENTER,
            word,
            font.clone(),
            edge,
        );
    }
    painter.text(
        at,
        egui::Align2::CENTER_CENTER,
        word,
        font,
        theme::p().map_ink,
    );
}

/// The camera-mode disc: the current mode's glyph and its word under it, a
/// tap steps to the next mode on offer — Follow only in a signed-in session
/// (`free_camera::follow_available`); the same cycle Tab walks. With at most
/// three modes, cycling beats a popup: the disc always shows where you are,
/// and the tooltip says where a tap goes.
fn mode_disc(
    ui: &mut egui::Ui,
    camera_state: &mut crate::resources::CameraState,
    session: &crate::systems::session::SessionState,
    has_avatar: bool,
) {
    use crate::resources::CameraMode;
    use crate::systems::free_camera::{enter_mode, follow_available, next_camera_mode};
    use crate::systems::icons;
    let badge = |mode: CameraMode| match mode {
        CameraMode::Avatar => (icons::CAM_FOLLOW, "Follow"),
        CameraMode::Free => (icons::CAM_FLY, "Fly"),
        CameraMode::Orbit => (icons::CAM_EARTH, "Earth"),
    };
    let next = next_camera_mode(camera_state.mode, follow_available(session, has_avatar));
    let (icon, mut name) = badge(camera_state.mode);
    // First person is a sub-state of Follow, with Follow's glyph — but a
    // different view, and the disc is where that has to show.
    if camera_state.mode == CameraMode::Avatar && camera_state.first_person {
        name = "First person";
    }
    let (_, next_name) = badge(next);
    let word = if name == "First person" {
        "1st person"
    } else {
        name
    };
    ui.spacing_mut().item_spacing.y = 0.0;
    let resp = theme::round_button(
        ui,
        icon,
        &format!("{name} view — switch to {next_name}"),
        false,
    );
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(theme::ROUND_BUTTON_SIZE, MODE_WORD_H),
        egui::Sense::hover(),
    );
    word_under(ui.painter(), rect.center() + egui::vec2(0.0, 1.0), word);
    if resp.clicked() {
        enter_mode(camera_state, next);
        camera_state.mode = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented phone viewports (`ui-vocabulary.md`): 274 pt on an
    /// Android handset at `ui_scale` 1.5, ≈214 pt on the web phone tier — the
    /// narrower and the one layout tests keep forgetting. 600 pt is the
    /// height of the phone shots (412×900 px at 1.5).
    const PHONES: [f32; 2] = [274.0, 214.0];
    const PHONE_H: f32 = 600.0;

    /// The switch and the search bar are size-to-content `Area`s that nothing
    /// clamps; what they ask for is what they take, off the screen included.
    /// Both are measured from the viewport, so they fit by construction —
    /// this pins that the construction holds, margins and all.
    #[test]
    fn the_switch_and_the_bar_fit_a_phone() {
        for w in PHONES {
            let switch = (w - 2.0 * theme::OSD_MARGIN).min(SWITCH_MAX_W);
            assert!(switch + 2.0 * theme::OSD_MARGIN <= w, "{switch} in {w}");
            // Two segments that each still hold a word at the smallest size.
            assert!(
                (switch - 10.0) / 2.0 >= 80.0,
                "a segment of {switch} is too narrow"
            );
            let bar = (w - 2.0 * theme::OSD_MARGIN).min(SEARCH_MAX_W);
            assert!(bar + 2.0 * theme::OSD_MARGIN <= w);
            // The face and the words beside the magnifier.
            assert!(bar - FACE - 44.0 >= 90.0, "no room for the words in {bar}");
        }
    }

    /// A finger needs 48 dp; the switch is a 48 pt row, 72 dp on Android.
    #[test]
    fn the_switch_is_a_touch_row() {
        assert!(SWITCH_H - 8.0 >= theme::MIN_TOUCH_TARGET_DP / 1.5);
        assert!(FACE >= theme::MIN_TOUCH_TARGET_DP / 1.5);
    }

    /// The two edges everything else anchors against cover what is drawn on
    /// them, and the columns start under the bar.
    #[test]
    fn the_reserved_edges_clear_the_bar_and_the_switch() {
        assert!(reserved_top() >= theme::OSD_MARGIN + SEARCH_H);
        assert!(reserved_bottom() >= theme::OSD_MARGIN + SWITCH_H);
        assert_eq!(left_slot(0), reserved_top());
        assert_eq!(right_column(true, true).layers, reserved_top());
    }

    /// The right column is one column of discs that never touch: a word or a
    /// caption under a disc gets its own room before the next disc.
    #[test]
    fn the_right_column_does_not_overlap_itself() {
        for (locate, camera) in [(false, false), (true, false), (false, true), (true, true)] {
            let c = right_column(locate, camera);
            let mut tops = vec![c.layers, c.mode];
            tops.extend(c.locate);
            tops.extend(c.camera);
            for pair in tops.windows(2) {
                assert!(
                    pair[1] - pair[0] >= theme::ROUND_BUTTON_SIZE + theme::OSD_GAP,
                    "{tops:?}"
                );
            }
            assert!(c.bottom >= tops.last().copied().unwrap() + theme::ROUND_BUTTON_SIZE);
        }
        // The mode disc's word sits in the gap under it.
        let c = right_column(true, true);
        assert!(c.locate.unwrap() - c.mode >= theme::ROUND_BUTTON_SIZE + MODE_WORD_H);
    }

    /// Both columns, top and bottom, fit a phone with the switch under them:
    /// the left one (compass, lens, light, Say) ends well above the
    /// thumbstick's corner, and the right one ends above the tallest thing
    /// the right edge grows from the bottom (`map_osd`).
    #[test]
    fn the_columns_fit_a_phone_above_the_switch() {
        let left_bottom = left_slot(3) + theme::ROUND_BUTTON_SIZE;
        let right_bottom = right_column(true, true).bottom;
        for (bottom, what) in [(left_bottom, "left"), (right_bottom, "right")] {
            assert!(
                bottom + theme::OSD_GROUP_GAP < PHONE_H - reserved_bottom(),
                "the {what} column reaches {bottom} of {PHONE_H}"
            );
        }
    }
}
