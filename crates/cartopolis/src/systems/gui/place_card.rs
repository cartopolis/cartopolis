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
//! It follows the dock's suppression rules: driver mode, the on-screen
//! keyboard and (on a compact viewport) an open panel each own the screen the
//! card would float over. Suppression *hides* the card rather than closing it
//! — the place survives a trip through the Route panel.
//!
//! The component library draws it (`lib_place_card`); [`drive_place_card`]
//! spends the pick, works out the suppression and the camera's side
//! ([`PlaceView`]) and does the verb the library's card asks for, through
//! [`PlaceCard::act`].

use bevy::prelude::*;

use crate::grid::AnchoredCamera;
use crate::resources::{CameraState, RightPanel};
use crate::systems::geo_nav::{self, GeoNavState, Teleport};
use crate::systems::icons;
use crate::systems::ui_input::UiMetrics;
use uuid::Uuid;
use web_time::Instant;

/// A place the card can show: what the user picked, where it is, and the words
/// that described it on the row they picked it from.
#[derive(Clone, Debug, PartialEq)]
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
    pub from: Option<Origin>,
}

/// Where a marker's card opens in Community.
#[derive(Clone, Debug, PartialEq)]
pub enum OpenIn {
    /// A chat, by channel key: a person sharing their location, a chat's point.
    Chat(String),
    /// A community's post, on its community's page.
    Post { community: Uuid, post: Uuid },
}

impl OpenIn {
    /// The card's main button: it says where it goes.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Chat(_) => "Open chat",
            Self::Post { .. } => "Open in Community",
        }
    }

    #[must_use]
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Chat(_) => icons::CHAT,
            Self::Post { .. } => icons::GROUP,
        }
    }
}

/// Where a place card's place comes from, when a Community marker opened it.
#[derive(Clone, Debug, PartialEq)]
pub struct Origin {
    /// Over the name: "Garage 44 · post", "sanne · sharing location".
    pub line: String,
    pub open: OpenIn,
    /// A person sharing their location: Route here follows them
    /// (`GeoNavState::route_to_person`), as the chat's Route does. The chat's
    /// key and the person.
    pub person: Option<(String, String)>,
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
    /// When Save last saved this place: its ✔ for a moment.
    saved_at: Option<Instant>,
    /// What [`drive_place_card`] and the library's card pass each other.
    pub lib_view: PlaceView,
}

/// A verb on the place shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceAction {
    /// Route here — to a person sharing their location, follow them.
    RouteHere,
    /// Start a route here.
    RouteFrom,
    /// Save into Places.
    Save,
    /// Jump there (signed in only).
    Jump,
    /// Send the place to a chat or community (`bridges`, the app only).
    Share,
    /// A Community marker's way back into Community (`bridges`).
    OpenOrigin,
}

/// The side of the card the library cannot work out itself — the camera, the
/// session, what owns the bottom of the screen.
#[derive(Default, Clone, PartialEq, Debug)]
pub struct PlaceView {
    /// Something owns the bottom of the screen: the card steps aside.
    pub hidden: bool,
    /// How far the place is from the camera, when the camera is placed.
    pub distance: Option<String>,
    /// Signed in: Jump there is offered.
    pub online: bool,
    /// Save just saved it (its ✔).
    pub saved: bool,
    /// A verb the library's card asked for, done by [`drive_place_card`].
    pub action: Option<PlaceAction>,
}

impl PlaceCard {
    /// Show a place, at the peek.
    pub fn open(&mut self, place: CardPlace) {
        self.place = Some(place);
        self.rest = crate::systems::motion::Rest::Peek;
        self.saved_at = None;
    }

    /// Put the card away.
    pub fn close(&mut self) {
        self.place = None;
    }

    /// Save saved this place within the last moment.
    #[must_use]
    pub fn saved(&self) -> bool {
        self.saved_at
            .is_some_and(|at| at.elapsed().as_secs_f32() < 1.5)
    }

    /// Do a verb on `place`. Share and OpenOrigin are the Community side's
    /// ([`to_bridges`]), not done here.
    #[allow(clippy::too_many_arguments)]
    pub fn act(
        &mut self,
        action: PlaceAction,
        place: &CardPlace,
        nav: &mut GeoNavState,
        store: &mut crate::systems::user_store::UserStore,
        camera_state: &CameraState,
        altitude: Option<f64>,
    ) {
        match action {
            PlaceAction::RouteHere => match place.from.as_ref().and_then(|f| f.person.as_ref()) {
                Some((key, who)) => nav.route_to_person(key, who, place.lat, place.lng),
                None => nav.route_to(&place.name, place.lat, place.lng),
            },
            PlaceAction::RouteFrom => nav.route_from(&place.name, place.lat, place.lng),
            PlaceAction::Save => {
                if self.saved() {
                    return;
                }
                // The name the user just read, not a reverse lookup — the
                // row's name is the better one. Altitude and pose come from
                // the camera, as every saved place's do: a place is a
                // remembered view as well as a coordinate.
                store.add_bookmark(
                    place.name.clone(),
                    place.lat,
                    place.lng,
                    altitude.unwrap_or(geo_nav::MIN_TELEPORT_ALTITUDE),
                    geo_nav::CameraPose::capture(camera_state),
                );
                self.saved_at = Some(Instant::now());
            }
            PlaceAction::Jump => {
                nav.pending_label = Some(place.name.clone());
                nav.pending_teleport = Some(Teleport::geo(place.lat, place.lng, None));
            }
            PlaceAction::Share | PlaceAction::OpenOrigin => {}
        }
    }
}

/// The verbs that reach Community (`bridges`): Share hands the place to the
/// Share sheet, OpenOrigin opens where a marker's place came from.
#[cfg(feature = "comms")]
pub fn to_bridges(
    action: PlaceAction,
    place: &CardPlace,
    bridges: &mut crate::systems::bridges::Bridges,
) {
    match action {
        PlaceAction::Share => {
            bridges.share = Some(crate::systems::bridges::SharePlace {
                name: place.name.clone(),
                lat: place.lat,
                lng: place.lng,
                view: None,
            });
        }
        PlaceAction::OpenOrigin => {
            if let Some(from) = &place.from {
                bridges.open = Some(from.open.clone());
            }
        }
        _ => {}
    }
}

/// The place card's state, for the library's card above the dock: spends a
/// pick, publishes [`PlaceView`], does the verb asked for. See the module note.
#[allow(clippy::too_many_arguments)]
pub fn drive_place_card(
    metrics: Res<UiMetrics>,
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
    #[cfg(feature = "comms")] mut bridges: ResMut<crate::systems::bridges::Bridges>,
) {
    // A pick made in the Search panel: take it, close the panel that made it.
    // The panel closing here rather than in `search_ui` is what keeps the rule
    // in one place — every path that opens the card is a path that closes the
    // panel, or the card floats over the list it came from.
    if let Some(place) = nav.pending_card.take() {
        card.open(place);
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
    // The Share sheet stands where the card does, and is about it.
    #[cfg(feature = "comms")]
    let sharing = bridges.share.is_some();
    #[cfg(not(feature = "comms"))]
    let sharing = false;
    let hidden = driver.active
        || keyboard.is_open()
        || menu.open
        || (metrics.compact() && !right_panel.is(RightPanel::None))
        || right_panel.is(RightPanel::Route)
        || sharing;
    let cam = camera.get();
    let viewer = cam
        .zip(map_anchor.geo)
        .map(|((cell, tf), anchor)| geo_nav::camera_geo(anchor, &globe, cell, tf));
    let altitude = cam.map(|(cell, tf)| globe.altitude_m(cell, tf));

    // Hand the library's card the side only this system can work out, and do
    // the verb it asked for. Read before taking, so a quiet frame leaves the
    // resource unchanged.
    let action = if card.lib_view.action.is_some() {
        card.lib_view.action.take()
    } else {
        None
    };
    let view = PlaceView {
        hidden,
        distance: card
            .place
            .as_ref()
            .zip(viewer)
            .map(|(p, here)| geo_nav::distance_label(here, (p.lat, p.lng))),
        online: session.is_online(),
        saved: card.saved(),
        action: None,
    };
    if card.lib_view != view {
        card.lib_view = view;
    }
    if let (Some(a), Some(place)) = (action, card.place.clone()) {
        card.act(a, &place, &mut nav, &mut store, &camera_state, altitude);
        #[cfg(feature = "comms")]
        to_bridges(a, &place, &mut bridges);
    }
}
