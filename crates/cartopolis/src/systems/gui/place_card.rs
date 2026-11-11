//! The place card: what a picked search result opens instead of a panel.
//!
//! A phone map's place page, sized to what this client can do with a place. The
//! search panel used to stay open after a pick, covering the destination it had
//! just flown you to; now the pick closes the panel, the map shows the place,
//! and this card rises above the dock carrying the verbs a place affords:
//!
//! - **Route from / Route to** — seed the planner's endpoints through the same
//!   [`RouteRequest`](crate::systems::geo_nav::RouteRequest) queue every other
//!   destination source uses. Picking the *other* endpoint is the Search panel
//!   again — that is deliberate reuse, not a missing picker: search already has
//!   the recents, the coordinate parser and the history, and a second endpoint
//!   picker would drift from it.
//! - **Save** — the place into Places, under the name the user just read on the
//!   row they tapped (no reverse-geocode round trip: we hold the better name).
//! - **Beam** — bring the avatar here, *signed in only*. Signed in, the row
//!   pick moves only the view ([`Teleport::look`]): the avatar is "My position"
//!   to the planner, and a pick that brought it along made "To" a route from
//!   the place to itself. Offline the pick moves the view and the local avatar
//!   with it, as there is no body in a shared world to keep apart from it.
//!
//! It is a floating `Area` like the dock and the main menu, deliberately not in
//! the panel chain, and it follows the dock's suppression rules: driver
//! mode, the on-screen keyboard and (on a compact viewport) an open panel each
//! own the screen the card would float over. Suppression *hides* the card rather
//! than closing it — the place survives a trip through the Route panel.

use crate::systems::egui_manager::EguiRef;
use bevy::prelude::*;

use crate::grid::AnchoredCamera;
use crate::resources::{CameraState, RightPanel};
use crate::systems::geo_nav::{self, GeoNavState, Teleport};
use crate::systems::icons;
use crate::systems::theme;
use web_time::Instant;

/// A place the card can show: what the user picked, where it is, and the words
/// that described it on the row they picked it from.
#[derive(Clone, Debug)]
pub struct CardPlace {
    pub name: String,
    /// What kind of place it is ("town", "restaurant", "recent") — the small
    /// tag the suggestion row showed beside the name.
    pub kind: Option<String>,
    /// The address context under the name, possibly empty.
    pub context: String,
    pub lat: f64,
    pub lng: f64,
    /// A Community marker opened it (bridge ①, `bridges`): where it comes
    /// from, said on top, and Open chat / Open in Community as its main verb.
    pub from: Option<crate::systems::bridges::Origin>,
}

/// The card's state: the place being shown, if any.
///
/// A resource rather than a `Local` because two systems meet here — the Search
/// panel queues a pick ([`GeoNavState::pending_card`]) and this card spends it.
#[derive(Resource, Default)]
pub struct PlaceCard {
    pub place: Option<CardPlace>,
    /// How much of the card is up: the name and Route here, or the rest too.
    pub rest: crate::systems::motion::Rest,
    /// The finger on its grabber.
    pub drag: crate::systems::motion::SheetDrag,
}

/// The card's two rests: there is not enough on a place for a third.
const RESTS: [crate::systems::motion::Rest; 2] = [
    crate::systems::motion::Rest::Peek,
    crate::systems::motion::Rest::Full,
];

/// Width of the card. Fixed for the reason the main menu's is: this is an
/// anchored, size-to-content `Area`, and content that reaches for the available
/// width grows the card a little every frame.
const CARD_WIDTH: f32 = 300.0;

/// The place card, above the dock. See the module note.
#[allow(clippy::too_many_arguments)]
pub fn render_place_card(
    egui: EguiRef,
    mut card: ResMut<PlaceCard>,
    mut nav: ResMut<GeoNavState>,
    mut right_panel: ResMut<RightPanel>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
    session: Res<crate::systems::session::SessionState>,
    menu: Res<crate::systems::ui::MainMenu>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    globe: Res<crate::systems::globe::GlobeState>,
    map_anchor: Res<crate::resources::MapAnchor>,
    camera: AnchoredCamera,
    camera_state: Res<CameraState>,
    mut saved_at: Local<Option<Instant>>,
    mut bridges: ResMut<crate::systems::bridges::Bridges>,
) {
    // A pick made in the Search panel: take it, close the panel that made it.
    // The panel closing here rather than in `search_ui` is what keeps the rule
    // in one place — every path that opens the card is a path that closes the
    // panel, or the card floats over the list it came from.
    if let Some(place) = nav.pending_card.take() {
        card.place = Some(place);
        card.rest = crate::systems::motion::Rest::Peek;
        card.drag = Default::default();
        *saved_at = None;
        if right_panel.is(RightPanel::Search) {
            *right_panel = RightPanel::None;
        }
    }
    // The dock's own suppression rules, and for the dock's reasons: each of
    // these owns the bottom of the screen, and an `Area` cannot be put behind a
    // panel. Hidden, not closed — the place is still the subject when the
    // keyboard or the panel goes away. And the route planner at any width: it
    // is a bottom sheet now (`navigation::SheetDetent`), anchored to the same
    // centre-bottom this card is, so on a desktop window the two would be drawn
    // on top of each other. Hidden rather than closed, as above — "Route to" is
    // a verb on *this* place, and it is still the subject when the planner goes
    // away.
    let hidden = driver.active
        || keyboard.is_open()
        || menu.open
        || (egui.compact() && !right_panel.is(RightPanel::None))
        || right_panel.is(RightPanel::Route)
        // The Share sheet stands where the card does, and is about it.
        || bridges.share.is_some();
    // The card rises from the dock and sinks back (`motion::linger`): the place
    // it was showing is still drawn, untouchable, for the last `motion::LEAVE`
    // after `card.place` is cleared.
    let ctx = egui.ctx();
    let Some((place, pres)) = crate::systems::motion::linger(
        ctx,
        egui::Id::new("place_card"),
        if hidden { None } else { card.place.clone() },
    ) else {
        return;
    };

    let cam = camera.get();
    let viewer = cam
        .zip(map_anchor.geo)
        .map(|((cell, tf), anchor)| geo_nav::camera_geo(anchor, &globe, cell, tf));
    let altitude = cam.map(|(cell, tf)| globe.altitude_m(cell, tf));

    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let mut close = false;
    egui::Area::new(egui::Id::new("place_card"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .interactable(pres.open)
        .show(ctx, |ui| {
            use crate::systems::motion::{self, Rest, SheetMove};
            let rest = card.rest;
            motion::follow(
                ui,
                egui::Id::new("place_card"),
                pres,
                &mut card.drag,
                motion::LIFT,
                rest == Rest::Full,
            );
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(CARD_WIDTH.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN));
                // The grabber: a pull up shows the rest, a pull or a flick down
                // puts the card away.
                match motion::grabber(ui, &mut card.drag, rest, &RESTS, "Expand or collapse") {
                    SheetMove::Stay => {}
                    SheetMove::To(rest) => card.rest = rest,
                    SheetMove::Close => close = true,
                }
                // Where a Community marker's place comes from, on top: the card
                // says which space it belongs to before what it is (N3).
                if let Some(from) = &place.from {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&from.line)
                                .font(egui::FontId::new(theme::SIZE_SUB, theme::brand_family()))
                                .color(theme::p().accent_text),
                        )
                        .truncate()
                        .selectable(false),
                    );
                }
                // Header: the name leads, the close button takes the corner. The
                // close button first — a truncating label asks for every point
                // available, so laid out the other way it pushes the ✕ off the
                // card (the rule every panel row here follows).
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = theme::close_button(ui).clicked();
                        let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
                        ui.allocate_ui_with_layout(
                            size,
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&place.name)
                                            .size(theme::SIZE_TITLE)
                                            .color(theme::p().cream),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                                if let Some(kind) = &place.kind {
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(kind).small().weak())
                                            .truncate()
                                            .selectable(false),
                                    );
                                }
                            },
                        );
                    });
                });
                // Context and distance share the dim line under the name, the
                // same shape the suggestion row used, so the card reads as the
                // row it came from.
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if let Some(here) = viewer {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(geo_nav::distance_label(
                                    here,
                                    (place.lat, place.lng),
                                ))
                                .small()
                                .color(theme::p().accent_text),
                            )
                            .selectable(false),
                        );
                    }
                    if !place.context.is_empty() {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&place.context).small().weak())
                                .truncate()
                                .selectable(false),
                        );
                    }
                });
                ui.add_space(6.0);

                // The verbs: one that is plainly the next step, and the rest
                // behind ⋯. They were four equal plates — From · To · Save ·
                // Beam — with From first, though getting *to* a place is what a
                // search for it is nearly always for (`docs/mockups/ux-review`).
                let saved = saved_at.is_some_and(|at| at.elapsed().as_secs_f32() < 1.5);
                // A Community marker: the way into Community is the main verb,
                // and the way there the other (N3). No Save, no Share: it is
                // somebody else's thing, not a place of the map's.
                if let Some(from) = &place.from {
                    let w = ui.available_width();
                    if theme::primary_button_sized(
                        ui,
                        &format!("{}  {}", from.open.icon(), from.open.verb()),
                        w,
                    )
                    .clicked()
                    {
                        bridges.open = Some(from.open.clone());
                    }
                    ui.add_space(2.0);
                    if theme::secondary_button(ui, icons::ROUTE, "Route here", w).clicked() {
                        match &from.person {
                            Some((key, who)) => nav.route_to_person(key, who, place.lat, place.lng),
                            None => nav.route_to(&place.name, place.lat, place.lng),
                        }
                    }
                    return;
                }
                // Route here across the card, and the place's own verbs as a row
                // of named buttons under it (N2): with Share beside Save and ⋯,
                // four in one row left "Route here" too narrow for its words on
                // a phone, and the card grew past the screen.
                let w = ui.available_width();
                if theme::primary_button_sized(ui, &format!("{}  Route here", icons::FINISH), w)
                    .clicked()
                {
                    nav.route_to(&place.name, place.lat, place.lng);
                }
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let cell = theme::equal_width(ui, 3);
                    let save_icon = if saved {
                        icons::CHECK
                    } else {
                        icons::BOOKMARK_ADD
                    };
                    let save = theme::secondary_button(
                        ui,
                        save_icon,
                        if saved { "Saved" } else { "Save" },
                        cell,
                    );
                    if save.on_hover_text("Save this place").clicked() && !saved {
                        // The name the user just read, not a reverse lookup —
                        // the row's name is the better one. Altitude and pose
                        // come from the camera, as every saved place's do: a
                        // place is a remembered view as well as a coordinate.
                        store.add_bookmark(
                            place.name.clone(),
                            place.lat,
                            place.lng,
                            altitude.unwrap_or(geo_nav::MIN_TELEPORT_ALTITUDE),
                            geo_nav::CameraPose::capture(&camera_state),
                        );
                        *saved_at = Some(Instant::now());
                    }
                    // Share (bridge ③): the one way a place goes to people.
                    // No orange dot on it, as N2 drew one: the person asked
                    // for little colour, and the word says it (2026-11-11).
                    let share = theme::secondary_button(ui, icons::SHARE, "Share", cell);
                    if share
                        .on_hover_text("Send this place to a chat or community")
                        .clicked()
                    {
                        bridges.share = Some(crate::systems::bridges::SharePlace {
                            name: place.name.clone(),
                            lat: place.lat,
                            lng: place.lng,
                            view: None,
                        });
                    }
                    let more = theme::secondary_button(ui, icons::MORE, "More", cell);
                    egui::Popup::menu(&more).show(|ui| {
                        if theme::menu_entry(ui, icons::START, "Start a route here").clicked() {
                            nav.route_from(&place.name, place.lat, place.lng);
                            ui.close();
                        }
                        // Signed in only: a body in a shared world is what makes
                        // "go there" distinct from looking. Offline the pick has
                        // already moved the view. "Jump there", the ring's word
                        // for it too, rather than "Beam".
                        if session.is_online()
                            && theme::menu_entry(ui, icons::CAM_FOLLOW, "Jump there").clicked()
                        {
                            nav.pending_label = Some(place.name.clone());
                            nav.pending_teleport = Some(Teleport::geo(place.lat, place.lng, None));
                            ui.close();
                        }
                    });
                });
                // Pulled up: what ⋯ holds, as rows, and where the place is.
                if card.rest == Rest::Full {
                    ui.add_space(6.0);
                    ui.separator();
                    if theme::menu_entry(ui, icons::START, "Start a route here").clicked() {
                        nav.route_from(&place.name, place.lat, place.lng);
                    }
                    if session.is_online()
                        && theme::menu_entry(ui, icons::CAM_FOLLOW, "Jump there").clicked()
                    {
                        nav.pending_label = Some(place.name.clone());
                        nav.pending_teleport = Some(Teleport::geo(place.lat, place.lng, None));
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("{:.5}, {:.5}", place.lat, place.lng))
                            .size(theme::SIZE_TINY)
                            .color(theme::p().text_dim),
                    );
                }
            });
        });
    if close {
        card.place = None;
    }
}
