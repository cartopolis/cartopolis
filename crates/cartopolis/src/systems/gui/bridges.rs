//! The three bridges between World and Community (`docs/mockups/redesign`,
//! v2, N1–N7), and no others:
//!
//!  1. **Markers → Community.** A Community thing with a place is a marker on
//!     the map (a person sharing their location, a community's post, a chat's
//!     point). A tap opens the place card with where it comes from on top
//!     ("sanne · sharing location", "Garage 44 · post") and **Open chat** /
//!     **Open in Community** ([`OpenIn`]), with Route here beside it.
//!  2. **Show in world.** Anything in Community with a place — a shared
//!     location, a point or a meet-up in a chat, a plan in the calendar, a
//!     post — has one button that switches to World, flies there, shows the
//!     place's card and puts **‹ Back to <it>** where the search bar was
//!     ([`Back`]). The chip goes when the person searches, opens another place,
//!     goes to Community by the switch, or moves the view away from the place
//!     (see [`Back::left`]).
//!  3. **Share.** In World a place's card has one Share, which opens one sheet:
//!     "Share <name> — Sends the place to a chat or community", the recent
//!     chats and the communities you can post in, and Copy link
//!     ([`drive_share_sheet`]). The search panel's "Send this view to a chat"
//!     and "Post this view to a community" and the ring's "Post here" lead
//!     here now.
//!
//! Sending into a private chat goes through the chat's own sealing queue
//! (`ChatState::private_points`), as "Send this view" always did — nothing
//! here touches the wire or the encryption (`encrypted-chat.md`).

use bevy::prelude::*;
use cartopolis_core::point::{Point, PointView};
use uuid::Uuid;

use crate::resources::RightPanel;
use crate::systems::theme;

// The two a place card carries live with it (`place_card`), so the web
// build, which has no bridges, still has the card.
pub use crate::systems::place_card::{OpenIn, Origin};

/// The Community screen Show in world left, to go back to exactly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Return {
    pub chat: Option<String>,
    pub you: bool,
    pub sub: Option<crate::systems::contacts::Sub>,
    pub community: Option<crate::systems::communities::Screen>,
    pub calendar: Option<(
        crate::systems::calendar::Page,
        Option<crate::systems::calendar::Pick>,
    )>,
}

/// The ‹ Back chip of a Show in world: what it says, where it goes, and the
/// place it was about.
#[derive(Clone, Debug, PartialEq)]
pub struct Back {
    pub label: String,
    pub to: Return,
    pub lat: f64,
    pub lng: f64,
    /// The view has reached the place; from then on leaving it ends the chip.
    pub arrived: bool,
}

impl Back {
    /// How far from the place the view may wander before the chip goes: a
    /// kilometre and a half, or three times the camera's height, whichever is
    /// more — so a look round the square keeps it, and flying off to somewhere
    /// else does not. Before the flight has arrived it never counts.
    #[must_use]
    pub fn left(&mut self, distance_m: f64, altitude_m: f64) -> bool {
        let reach = 1500.0_f64.max(3.0 * altitude_m);
        if distance_m <= reach {
            self.arrived = true;
            return false;
        }
        self.arrived
    }
}

/// A place being shared (bridge 3).
#[derive(Clone, Debug, PartialEq)]
pub struct SharePlace {
    pub name: String,
    pub lat: f64,
    pub lng: f64,
    /// The view to open it at: Search's "this view" carries one, a place card
    /// does not.
    pub view: Option<PointView>,
}

impl SharePlace {
    #[must_use]
    pub fn point(&self) -> Point {
        Point {
            latitude: self.lat,
            longitude: self.lng,
            name: Some(self.name.clone()),
            text: None,
            view: self.view.clone(),
            meetup: None,
        }
    }

    #[must_use]
    pub fn post_place(&self) -> cartopolis_core::posts::PostPlace {
        cartopolis_core::posts::PostPlace {
            latitude: self.lat,
            longitude: self.lng,
            name: Some(self.name.clone()),
            view: self.view.clone(),
        }
    }

    #[must_use]
    pub fn link(&self) -> String {
        let (alt, look) = match &self.view {
            Some(v) => (v.altitude, Some((v.pitch, v.yaw))),
            None => (300.0, None),
        };
        crate::systems::place_link::web_link(self.lat, self.lng, alt, look)
    }
}

/// The bridges' state. Written by the place card (Open), the chats (Show in
/// world, through `ChatsView::show_in_world`), the dock (the chip's tap), the
/// search panel, the ring and the cards (Share); spent by [`drive_bridges`].
#[derive(Resource, Default)]
pub struct Bridges {
    /// A marker card's Open: switch to Community there.
    pub open: Option<OpenIn>,
    /// The way back from Show in world.
    pub back: Option<Back>,
    /// The chip (or Back) was pressed: go back.
    pub back_pressed: bool,
    /// The Share sheet is up for this place.
    pub share: Option<SharePlace>,
    /// When Copy link last fired (`Time<Real>` seconds), for its ✓.
    copied_at: Option<f64>,
    /// The Share sheet as the library draws it (`lib_share`), while it is up.
    pub lib_view: Option<ShareView>,
    /// The library's taps, done by [`drive_share_sheet`] next frame.
    pub lib_acts: Vec<ShareAct>,
}

/// The Share sheet as the library draws it (`lib_share`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct ShareView {
    pub spot: crate::systems::ui::SheetSpot,
    /// The place's name: "Share <name>".
    pub name: String,
    pub signed_in: bool,
    /// The chats to send to: (key, name, a community's topic).
    pub chats: Vec<(String, String, bool)>,
    /// The communities to post in.
    pub communities: Vec<(Uuid, String)>,
    /// Copy link fired a moment ago: its ✓.
    pub copied: bool,
}

/// A tap on the library's Share sheet.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum ShareAct {
    Close,
    /// Into this chat.
    Send(String),
    /// As a post in this community.
    Post(Uuid),
    Copy,
}

/// The screens a bridge reads and writes, bundled under Bevy's parameter cap.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Screens<'w> {
    view: ResMut<'w, crate::systems::chats::ChatsView>,
    chat: ResMut<'w, crate::systems::chat::ChatState>,
    communities: ResMut<'w, crate::systems::communities::Communities>,
    calendar: ResMut<'w, crate::systems::calendar::Calendar>,
}

/// Carry out what the bridges were asked: open in Community, Show in world,
/// go back, the ring's Share; and end the ‹ Back chip when its time is up.
#[allow(clippy::too_many_arguments)]
pub fn drive_bridges(
    mut bridges: ResMut<Bridges>,
    mut right_panel: ResMut<RightPanel>,
    mut s: Screens,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    mut place_card: ResMut<crate::systems::place_card::PlaceCard>,
    mut ring: ResMut<crate::resources::ContextMenuState>,
    camera: crate::grid::AnchoredCamera,
    globe: Res<crate::systems::globe::GlobeState>,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    // The ring's Share, for the spot it was opened on.
    if let Some((lat, lng, name)) = ring.share_at.take() {
        bridges.share = Some(SharePlace {
            name,
            lat,
            lng,
            view: None,
        });
    }

    // ① A marker's card: Open chat / Open in Community.
    if let Some(open) = bridges.open.take() {
        open_in_community(&open, &mut s);
        *right_panel = RightPanel::Chat;
        place_card.place = None;
        bridges.back = None;
    }

    // ② Show in world.
    if let Some(point) = s.view.show_in_world.take() {
        let to = Return {
            chat: s.view.open.clone(),
            you: s.view.you,
            sub: s.view.sub.clone(),
            community: s.communities.screen.clone(),
            calendar: s
                .calendar
                .open
                .then(|| (s.calendar.page, s.calendar.picked.clone())),
        };
        let label = back_label(&to, &s, &point);
        nav.pending_label = point.name.clone();
        nav.pending_teleport = Some(crate::systems::geo_nav::Teleport::point(&point));
        // The place's card, not the plan's or the post's (N7).
        nav.pending_card = Some(crate::systems::place_card::CardPlace {
            name: point.name.clone().unwrap_or_else(|| label.clone()),
            kind: None,
            context: String::new(),
            lat: point.latitude,
            lng: point.longitude,
            from: None,
        });
        s.chat.world = None;
        *right_panel = RightPanel::None;
        bridges.back = Some(Back {
            label,
            to,
            lat: point.latitude,
            lng: point.longitude,
            arrived: false,
        });
    }

    // ‹ Back, by the chip or by Back.
    if std::mem::take(&mut bridges.back_pressed) {
        if let Some(back) = bridges.back.take() {
            let to = back.to;
            s.view.open = to.chat.clone();
            s.view.you = to.you;
            s.view.sub = to.sub;
            s.communities.screen = to.community;
            match to.calendar {
                Some((page, picked)) => {
                    s.calendar.open = true;
                    s.calendar.page = page;
                    s.calendar.picked = picked;
                }
                None => s.calendar.open = false,
            }
            if let Some(key) = &to.chat {
                s.chat.focus_channel(key);
            }
            place_card.place = None;
            *right_panel = RightPanel::Chat;
        }
    }

    // The chip's lifetime: a search, another place, Community by the switch,
    // or a view that has left the place.
    let Some(back) = bridges.back.as_mut() else {
        return;
    };
    let other_place = place_card
        .place
        .as_ref()
        .is_some_and(|p| (p.lat - back.lat).abs() > 1e-6 || (p.lng - back.lng).abs() > 1e-6);
    let elsewhere = right_panel.is(RightPanel::Search) || right_panel.is(RightPanel::Chat);
    let left = camera
        .get()
        .zip(map_anchor.geo)
        .is_some_and(|((cell, tf), anchor)| {
            let here = crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf);
            let d = crate::systems::geo_nav::great_circle_m(here, (back.lat, back.lng));
            back.left(d, globe.altitude_m(cell, tf))
        });
    if other_place || elsewhere || left {
        bridges.back = None;
    }
}

/// Switch the Community panel's state to where `open` points.
fn open_in_community(open: &OpenIn, s: &mut Screens) {
    s.view.you = false;
    s.view.sub = None;
    s.view.new_chat = false;
    s.calendar.open = false;
    s.chat.world = None;
    match open {
        OpenIn::Chat(key) => {
            s.communities.screen = None;
            s.view.open_chat(key);
            s.chat.focus_channel(key);
        }
        OpenIn::Post { community, post } => {
            s.view.open = None;
            s.communities.posts.focus(*community, *post);
            s.communities.screen = Some(crate::systems::communities::Screen::Community(*community));
        }
    }
}

/// What the chip calls the screen it goes back to: the chat, the community,
/// the plan — what the person was looking at, by its name.
fn back_label(to: &Return, s: &Screens, point: &Point) -> String {
    if let Some((_, Some(pick))) = &to.calendar {
        if let Some(name) = s.calendar.title_of(pick) {
            return name;
        }
    }
    if let Some(crate::systems::communities::Screen::Community(id)) = &to.community {
        if let Some(c) = s.communities.get(*id) {
            return c.settings.name.clone();
        }
    }
    if let Some(key) = &to.chat {
        if let Some(c) = s.chat.channels.iter().find(|c| &c.key == key) {
            return crate::systems::chats::display_name(key, &c.label);
        }
    }
    if to.calendar.is_some() {
        return "Calendar".into();
    }
    point.name.clone().unwrap_or_else(|| "Community".into())
}

/// The Share sheet (bridge 3): one sheet over the map that says what it does,
/// the recent chats and the communities to send the place to, and Copy link.
/// A card standing on the World | Community switch, as your face's sheet is.
/// Decides what it offers and where it stands, publishes it for the library
/// (`lib_share`) and does the taps handed back.
#[allow(clippy::too_many_arguments)]
pub fn drive_share_sheet(
    metrics: Res<crate::systems::ui_input::UiMetrics>,
    ui_input: Res<crate::systems::ui_input::UiInput>,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time<Real>>,
    mut bridges: ResMut<Bridges>,
    mut right_panel: ResMut<RightPanel>,
    mut s: Screens,
    online: Option<Res<crate::resources::OnlineSession>>,
    // Whether the pointer is over one of the library's screens, for the
    // sheet's tap-outside test (`ui::drive_main_menu`'s).
    #[cfg(feature = "ui-probe")] lib_pointer: Option<Res<mithril_bevy::UiPointer>>,
) {
    let lib_acts = std::mem::take(&mut bridges.lib_acts);
    let Some(place) = bridges.share.clone() else {
        if bridges.lib_view.is_some() {
            bridges.lib_view = None;
        }
        return;
    };
    let signed_in = online.is_some() || s.view.demo;
    let viewport = metrics.viewport_pts();
    let width = if metrics.compact() {
        viewport.x - 2.0 * theme::OSD_MARGIN
    } else {
        340.0_f32.min(viewport.x - 2.0 * theme::OSD_MARGIN)
    };
    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let max_h = (viewport.y - bottom - theme::OSD_MARGIN).max(160.0);
    let now = time.elapsed_secs_f64();
    let copied = bridges.copied_at.is_some_and(|t| now - t < 1.5);

    // The chats to send to: the private ones (people and groups, and a
    // community's topics), newest first. A public channel is not a place to
    // drop somebody's location in.
    // People and groups first, then a community's topics, each newest first.
    let mut chats: Vec<(String, String, bool, u64)> = s
        .chat
        .channels
        .iter()
        .filter(|c| crate::systems::chat::ChatState::is_private(&c.key))
        .map(|c| {
            let name = crate::systems::chats::display_name(&c.key, &c.label);
            // A community's topic is named "#topic"; a person or a group is not.
            let topic = name.starts_with('#');
            (
                c.key.clone(),
                name,
                topic,
                c.lines.last().map_or(0, |l| l.at_ms),
            )
        })
        .collect();
    chats.sort_by(|a, b| a.2.cmp(&b.2).then(b.3.cmp(&a.3)));
    chats.truncate(4);
    let communities: Vec<(Uuid, String)> = s
        .communities
        .list
        .iter()
        .filter(|c| c.role.is_some())
        .take(3)
        .map(|c| (c.id, c.settings.name.clone()))
        .collect();

    let view = ShareView {
        spot: crate::systems::ui::SheetSpot::Bottom {
            left: ((viewport.x - width) / 2.0).max(0.0),
            bottom,
            width,
            max_height: max_h,
        },
        name: place.name.clone(),
        signed_in,
        chats: chats
            .into_iter()
            .map(|(key, name, topic, _)| (key, name, topic))
            .collect(),
        communities,
        copied,
    };
    // The tap that put it up (a card's Share) is not a tap off it.
    let fresh = bridges.lib_view.is_none();
    if bridges.lib_view.as_ref() != Some(&view) {
        bridges.lib_view = Some(view);
    }

    let mut close = false;
    let mut send_to: Option<String> = None;
    let mut post_in: Option<Uuid> = None;
    let mut copy = false;
    for a in lib_acts {
        match a {
            ShareAct::Close => close = true,
            ShareAct::Send(key) => send_to = Some(key),
            ShareAct::Post(id) => post_in = Some(id),
            ShareAct::Copy => copy = true,
        }
    }
    // A tap off the sheet puts it away, as off your face's sheet. Outside is
    // on none of the library's screens: the sheet is one. A touch arrives as
    // the emulated mouse (`touch-input.md`).
    #[cfg(feature = "ui-probe")]
    let over_lib = lib_pointer.is_some_and(|p| p.owns_pointer());
    #[cfg(not(feature = "ui-probe"))]
    let over_lib = ui_input.overlay_owns_pointer();
    #[cfg(feature = "ui-probe")]
    let _ = &ui_input;
    let clicked_out = !fresh && !over_lib && mouse.just_pressed(MouseButton::Left);

    if copy {
        crate::utils::clipboard::set_text(&place.link());
        bridges.copied_at = Some(now);
    }
    if let Some(key) = send_to {
        // Into the chat's own sealing queue, as "Send this view" always was,
        // and on to the chat, so the person sees it arrive.
        s.chat.private_points.push((key.clone(), place.point()));
        s.view.you = false;
        s.view.sub = None;
        s.communities.screen = None;
        s.calendar.open = false;
        s.view.open_chat(&key);
        s.chat.focus_channel(&key);
        *right_panel = RightPanel::Chat;
        close = true;
    }
    if let Some(id) = post_in {
        // The community's composer over the map, with the place on the post —
        // where the ring's Post here used to open it.
        s.communities.posts.from_view = Some(place.post_place());
        s.communities.posts.compose(id);
        s.communities.posts.over_map = true;
        *right_panel = RightPanel::None;
        close = true;
    }
    if close || clicked_out {
        bridges.share = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chip holds while the view is at the place, and goes once it has
    /// left — but never before the flight has arrived.
    #[test]
    fn the_back_chip_goes_when_the_view_leaves_the_place() {
        let mut back = Back {
            label: "Vismarkt dinner".into(),
            to: Return::default(),
            lat: 53.2,
            lng: 6.5,
            arrived: false,
        };
        assert!(!back.left(40_000.0, 1500.0), "still flying there");
        assert!(!back.left(200.0, 300.0), "arrived");
        assert!(!back.left(1200.0, 300.0), "a look round the square");
        assert!(back.left(5000.0, 300.0), "somewhere else");
        // From high up the place is still in view farther off.
        let mut high = Back {
            arrived: true,
            ..back.clone()
        };
        assert!(!high.left(5000.0, 2500.0));
    }

    #[test]
    fn each_card_says_where_it_goes() {
        assert_eq!(OpenIn::Chat("k".into()).verb(), "Open chat");
        assert_eq!(
            OpenIn::Post {
                community: Uuid::nil(),
                post: Uuid::nil()
            }
            .verb(),
            "Open in Community"
        );
    }

    /// A shared place opens where it was shared: the link carries the view
    /// when there is one, and the point the name.
    #[test]
    fn a_shared_place_carries_its_name_and_view() {
        let place = SharePlace {
            name: "Martinitoren".into(),
            lat: 53.2194,
            lng: 6.5681,
            view: None,
        };
        assert_eq!(place.point().name.as_deref(), Some("Martinitoren"));
        assert_eq!(place.post_place().name.as_deref(), Some("Martinitoren"));
        assert!(place
            .link()
            .starts_with(crate::systems::place_link::WEB_ORIGIN));
    }
}
