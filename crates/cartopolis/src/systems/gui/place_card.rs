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
//! - **Teleport** — bring the avatar here, *signed in only*. Offline the row
//!   pick has already moved the view (and the local avatar rides the re-anchor);
//!   a body you share a world with is what makes "go there" a distinct act.
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
}

/// The card's state: the place being shown, if any.
///
/// A resource rather than a `Local` because two systems meet here — the Search
/// panel queues a pick ([`GeoNavState::pending_card`]) and this card spends it.
#[derive(Resource, Default)]
pub struct PlaceCard {
    pub place: Option<CardPlace>,
}

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
) {
    // A pick made in the Search panel: take it, close the panel that made it.
    // The panel closing here rather than in `search_ui` is what keeps the rule
    // in one place — every path that opens the card is a path that closes the
    // panel, or the card floats over the list it came from.
    if let Some(place) = nav.pending_card.take() {
        card.place = Some(place);
        *saved_at = None;
        if right_panel.is(RightPanel::Search) {
            *right_panel = RightPanel::None;
        }
    }
    let Some(place) = card.place.clone() else {
        return;
    };
    // The dock's own suppression rules, and for the dock's reasons: each of
    // these owns the bottom of the screen, and an `Area` cannot be put behind a
    // panel. Hidden, not closed — the place is still the subject when the
    // keyboard or the panel goes away.
    if driver.active || keyboard.is_open() || menu.open {
        return;
    }
    if egui.compact() && !right_panel.is(RightPanel::None) {
        return;
    }
    // And the route planner at any width: it is a bottom sheet now
    // (`navigation::SheetDetent`), anchored to the same centre-bottom this card
    // is, so on a desktop window the two would be drawn on top of each other.
    // Hidden rather than closed, as above — "Route to" is a verb on *this* place,
    // and it is still the subject when the planner goes away.
    if right_panel.is(RightPanel::Route) {
        return;
    }

    let cam = camera.get();
    let viewer = cam
        .zip(map_anchor.geo)
        .map(|((cell, tf), anchor)| geo_nav::camera_geo(anchor, &globe, cell, tf));
    let altitude = cam.map(|(cell, tf)| globe.altitude_m(cell, tf));

    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let ctx = egui.ctx();
    let mut close = false;
    egui::Area::new(egui::Id::new("place_card"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(CARD_WIDTH.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN));
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
                                            .strong()
                                            .size(theme::SIZE_SUB),
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
                                .color(theme::ACCENT_TEXT),
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

                // The verbs. One row, justified, so the targets line up and stay
                // finger-sized; each is icon + word because four bare icons in a
                // row is a puzzle, and four sentences do not fit.
                let saved = saved_at.is_some_and(|at| at.elapsed().as_secs_f32() < 1.5);
                ui.horizontal(|ui| {
                    let n = if session.is_online() { 4.0 } else { 3.0 };
                    let w = (ui.available_width() - (n - 1.0) * ui.spacing().item_spacing.x) / n;
                    let verb = |ui: &mut egui::Ui, icon: &str, label: &str, hover: &str| {
                        ui.add_sized(
                            egui::vec2(w, ui.spacing().interact_size.y),
                            egui::Button::new(format!("{icon} {label}")),
                        )
                        .on_hover_text(hover)
                        .clicked()
                    };
                    if verb(
                        ui,
                        icons::START,
                        "From",
                        "Route from here — make this the start",
                    ) {
                        nav.route_from(&place.name, place.lat, place.lng);
                    }
                    if verb(
                        ui,
                        icons::FINISH,
                        "To",
                        "Route here — make this the destination",
                    ) {
                        nav.route_to(&place.name, place.lat, place.lng);
                    }
                    let save_icon = if saved {
                        icons::CHECK
                    } else {
                        icons::BOOKMARK_ADD
                    };
                    if verb(ui, save_icon, "Save", "Add this place to Places") && !saved {
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
                    // Signed in only: a body in a shared world is what makes
                    // "go there" distinct from looking. Offline the pick has
                    // already moved the view.
                    if session.is_online()
                        && verb(ui, icons::CAM_FOLLOW, "Beam", "Teleport the avatar here")
                    {
                        nav.pending_label = Some(place.name.clone());
                        nav.pending_teleport = Some(Teleport::geo(place.lat, place.lng, None));
                    }
                });
            });
        });
    if close {
        card.place = None;
    }
}
