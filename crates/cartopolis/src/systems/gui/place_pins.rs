//! Map pins for saved places: the classic teardrop marker, drawn over the map
//! at each place's projected position and coloured by its folder
//! (`systems::bookmarks`, `systems::user_store`).
//!
//! **Why these are painted, not meshed.** The POI markers of a data layer are
//! real geometry — poles and gem heads standing in the street, sized in metres
//! (`systems::data_layers`). A saved place is not a thing in the world, it is a
//! bookmark *about* a spot, and it has to stay findable from 2 m and from 200 km.
//! Geometry can't do that: sized for the street it vanishes from altitude, sized
//! for altitude it is a tower at street level. So a pin is a fixed-size screen
//! overlay, exactly as every 2D map draws it, and it never occludes or is
//! occluded — it hovers over the map like a stuck-on marker.
//!
//! **Where it may be drawn.** egui paints floating `Area`s above panels, so pins
//! are culled against the free central area of the viewport
//! (`Ui::available_rect_before_wrap` after every side panel has taken its space,
//! which is why this system runs last of the UI systems). Without that, a pin
//! behind the Places panel would paint on top of the list it belongs to.
//!
//! **Staying put while the camera moves.** A pin is projected every frame, so it
//! is only as steady as its projection. Two things make one wobble and neither is
//! about precision in the usual sense: projecting through the camera's
//! `GlobalTransform` uses last frame's orientation (big_space propagates in
//! `PostUpdate`; this runs in `Update`), and egui rounds each pin's `Area` to a
//! whole physical pixel. Both are handled below — the camera basis is rebuilt from
//! this frame's `Transform`, and the silhouette is painted at the exact sub-pixel
//! tip rather than at the rounded rect. See `docs/notes/screen-overlay-projection.md`.
//! A third made them stand still and jump: on a frame with no input egui keeps
//! last frame's drawing, so every tip is recorded in a `WorldAnchored`, which asks
//! for the redraw when one moved.
//!
//! Below the space tier a place is placed through the anchored Mercator frame
//! (`crate::grid::lat_lng_to_world`) — the same frame the tiles are laid in, so the
//! pin sits on its building. In space that frame is meaningless, so the point is
//! taken on the globe itself (`globe::GlobeState::direction_local`) and pins on
//! the far side are dropped by the horizon test.
//!
//! **Two editions, one placing.** Where every marker goes, which are culled
//! and what a tap on one does are worked out here once; egui paints them, or
//! — with [`PlacePins::lib`] on — the component library does
//! (`systems::lib_pins`), from the list this system publishes in
//! [`PlacePins::lib_view`], and its taps come back through
//! [`PlacePins::lib_tapped`] to the same code a click on egui's pin runs.

use crate::components::FreeCamera;
#[cfg(feature = "comms")]
use crate::components::{Avatar, RemoteAvatar};
use crate::resources::MapAnchor;
#[cfg(feature = "comms")]
use crate::systems::chat_people;
use crate::systems::egui_manager::{EguiMut, WorldAnchored};
use bevy::ecs::system::SystemParam;
use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::grid::anchored_position_f64;
use crate::grid::lat_lng_to_world;
use crate::systems::egui_manager::EguiManager;
use crate::systems::geo_nav::{GeoNavState, Teleport};
use crate::systems::globe::GlobeState;
use crate::systems::theme;
use crate::systems::user_store::UserStore;

/// Radius of the pin's round head, in points. The body is drawn from it, so this
/// is the one number that sets the pin's size: overall height is `3 × HEAD_R`.
const HEAD_R: f32 = 7.5;
/// The head sits this many head-radii above the tip. At 2.0 the tangents from
/// the tip meet the circle at ±60°, which is the classic map-pin silhouette.
const HEAD_LIFT: f32 = 2.0;
/// White centre hole, as a fraction of the head radius.
const HOLE_R: f32 = 0.40;
/// Hovering scales the pin up by this much (and reveals its name).
const HOVER_SCALE: f32 = 1.18;
#[cfg(feature = "comms")]
/// First pin id used for a chat's points: far above any bookmark id, which
/// counts up from 1.
const CHAT_PIN_IDS: u64 = 1 << 48;
#[cfg(feature = "comms")]
/// Where a community's public posts' pins number from: a range of their own,
/// clear of the chat points (`1 << 48`). People are not pins at all
/// (`chat_people::marker`), so nothing numbers from `1 << 49` any more.
const POST_PIN_IDS: u64 = 1 << 50;

#[cfg(feature = "comms")]
/// The "you are sharing your real location" chip: top centre of the screen,
/// naming who can see it and for how long, with a Stop for each. Answers the
/// chat whose Stop was pressed.
///
/// Drawn from this system because it is the last UI system to run (the module
/// note says why), so nothing composed later can cover it.
fn sharing_chip(ctx: &egui::Context, lines: &[(String, String)]) -> Option<String> {
    let mut stop = None;
    egui::Area::new(egui::Id::new("real_location_sharing_chip"))
        // Under the search bar, which has the top edge.
        .anchor(
            egui::Align2::CENTER_TOP,
            egui::vec2(0.0, crate::systems::dock::reserved_top()),
        )
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::REAL_LOCATION.to_egui())
                .corner_radius(12.0)
                .inner_margin(egui::Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    for (channel, line) in lines {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(line).color(theme::DARK32.ink));
                            // A word on the location-sharing chip, whose fill is one
                            // colour in both themes, so its ink is the dark theme's
                            // whatever the palette.
                            // egui's own: the theme's buttons would take the palette's ink.
                            let word = egui::Button::new(
                                egui::RichText::new("Stop")
                                    .color(theme::DARK32.ink)
                                    .strong(),
                            );
                            if ui.add(word).clicked() {
                                stop = Some(channel.clone());
                            }
                        });
                    }
                });
        });
    stop
}

#[cfg(feature = "comms")]
/// The sharing chip's lines, one per chat this phone's location goes to: the
/// chat's key, and "◎ sanne sees where your phone is · 12 min". Both
/// editions' chips say these words (`lib_pins`).
fn sharing_lines(chat: &crate::systems::chat::ChatState) -> Vec<(String, String)> {
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    chat.sharing
        .iter()
        .map(|share| {
            let who = chat
                .channels
                .iter()
                .find(|c| c.key == share.channel)
                .map(|c| crate::systems::private_chat::peer_of_label(&c.label).to_string())
                .unwrap_or_else(|| "a chat".into());
            (
                share.channel.clone(),
                format!(
                    "{} {who} sees where your phone is · {} min",
                    crate::systems::icons::HERE,
                    share.until_ms.saturating_sub(now) / 60_000 + 1
                ),
            )
        })
        .collect()
}

/// The pins' own state: whether the component library draws them
/// (`systems::lib_pins`), and what goes back and forth with it. Inserted by
/// the library's system when it takes the pins over; absent, egui draws.
#[derive(Resource, Default)]
pub struct PlacePins {
    /// The library draws the markers; egui's system still places, culls and
    /// acts, and paints nothing.
    pub lib: bool,
    /// What the library draws this frame, written by [`render_place_pins`].
    pub lib_view: PinsView,
    /// The key of the marker the library's tap landed on ([`PinItem::key`]),
    /// done next frame as a click on egui's pin is.
    pub lib_tapped: Option<String>,
    /// A Stop on the library's sharing chip: the chat to stop telling.
    pub lib_stop: Option<String>,
}

/// Every marker of one frame, placed and culled, for the library.
#[derive(Default, Clone, Debug)]
pub struct PinsView {
    pub pins: Vec<PinItem>,
    /// What people last said, over their markers, after every marker.
    pub balloons: Vec<BalloonItem>,
    /// The "you are sharing your real location" chip's lines: the chat's
    /// key and what the line says.
    pub sharing: Vec<(String, String)>,
}

/// One marker where it stands this frame.
#[derive(Clone, Debug)]
pub struct PinItem {
    /// Stable across frames: what the library keys the marker's node by.
    pub key: String,
    /// The tip, in egui points (the library's px natively).
    pub tip: egui::Pos2,
    pub name: String,
    pub look: PinLook,
}

/// What a marker looks like: everything about it that does not move.
#[derive(Clone, Debug, PartialEq)]
pub enum PinLook {
    /// A saved place's teardrop, or a community post's, in its colour.
    Place { color: [f32; 3] },
    /// A chat's point: its number, and the label's width as egui measures it.
    #[cfg(feature = "comms")]
    Point { number: String, width: f32 },
    /// A person (`chat_people::marker`), with the width of the name's chip.
    #[cfg(feature = "comms")]
    Person {
        kind: chat_people::MarkerKind,
        chip: f32,
    },
}

/// A speech balloon over a person's marker (`chat_people::balloon`), placed:
/// its bottom-left corner, width and where its tail points from its left.
#[derive(Clone, Debug, PartialEq)]
pub struct BalloonItem {
    pub key: String,
    pub left: f32,
    pub bottom: f32,
    pub width: f32,
    pub tail: f32,
    pub text: String,
    pub alpha: f32,
    pub mine: bool,
}

/// What a tap on a marker does, resolved after every marker is placed: the
/// lists it indexes are this frame's.
#[derive(Clone)]
enum PinTap {
    /// `store.data.bookmarks[i]`.
    Bookmark(usize),
    /// Your own marker: fly there.
    #[cfg(feature = "comms")]
    Jump(Teleport, String),
    /// The chat's points, a community's posts and people's shared phones,
    /// by their index this frame.
    #[cfg(feature = "comms")]
    ChatPoint(usize),
    #[cfg(feature = "comms")]
    Post(usize),
    #[cfg(feature = "comms")]
    Phone(usize),
    /// A person's card, by username.
    #[cfg(feature = "comms")]
    Person(String),
}

/// The width `text` takes at `font`, as egui lays it out: what the library's
/// edition centres a label of unknown width by (its root has no box to centre
/// in, and a wide box would take the taps meant for what is under it).
/// Only people's markers and a chat's points need it (`comms`).
#[cfg_attr(not(feature = "comms"), allow(dead_code))]
pub(crate) fn text_width(ctx: &egui::Context, text: &str, font: egui::FontId) -> f32 {
    ctx.layer_painter(egui::LayerId::background())
        .layout_no_wrap(text.to_string(), font, egui::Color32::WHITE)
        .size()
        .x
}

/// The frame around the pins, bundled under Bevy's parameter cap.
#[derive(SystemParam)]
pub struct PinsFrame<'w> {
    keyboard: Res<'w, crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    lens: Res<'w, crate::systems::scenario::LensSlot>,
    driver: Res<'w, crate::systems::driver::DriverState>,
    route: Res<'w, crate::systems::navigation::RouteState>,
    lib: Option<ResMut<'w, PlacePins>>,
}

/// Beyond this ground distance a pin is dropped below the space tier. The
/// anchored Mercator frame distorts with distance from the anchor, and a pin
/// three provinces away is projected onto the horizon haze where it means
/// nothing — it is the globe's job to show those.
const MAX_FLAT_RANGE_M: f64 = 250_000.0;

/// Draw a pin per saved place and jump to the one that gets clicked.
///
/// Runs after every panel so `available_rect_before_wrap` is the map area (see
/// the module docs) and after the camera has moved, so pins sit on the frame the
/// user is actually looking at rather than the previous one.
pub fn render_place_pins(
    mut egui: EguiMut,
    store: Res<UserStore>,
    mut nav: ResMut<GeoNavState>,
    globe: Res<GlobeState>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    map_anchor: Res<MapAnchor>,
    // The keyboard, the driver bar, the route sheet, and the library's
    // edition when it has the pins ([`PlacePins`]).
    mut frame: PinsFrame,
    // The points of the private chat whose World view is on (the chat panel's
    // "Map" toggle). Drawn by the same code as saved places so they share the
    // projection, the culling and the one ordering rule this system obeys.
    //
    // Also where other people's shared real locations are pinned, and where the
    // "you are sharing your location" chip is drawn — see [`sharing_chip`].
    #[cfg(feature = "comms")] mut chat: ResMut<crate::systems::chat::ChatState>,
    // The people in that chat, where their avatars are (`chat_people`): the
    // server's answer, and this client's own avatars to stand a marker on
    // when one of them is in view.
    #[cfg(feature = "comms")] mut contacts: ResMut<crate::systems::contacts::Contacts>,
    #[cfg(feature = "comms")] remotes: Query<(&RemoteAvatar, &Transform), Without<FreeCamera>>,
    #[cfg(feature = "comms")] local: Query<
        (&GridCell, &Transform),
        (With<Avatar>, Without<FreeCamera>),
    >,
    // A community's public posts, when its page asked for them on the map.
    #[cfg(feature = "comms")] communities: Res<crate::systems::communities::Communities>,
    // Your own name, for the marker over your own avatar. Bevy's cap is
    // sixteen parameters: the next one goes into `PinsFrame`.
    #[cfg(feature = "comms")] identity: Option<Res<crate::resources::AuthIdentity>>,
) {
    // Where every marker went this frame, so egui redraws on the frames the map
    // moved under them and on no others (`WorldAnchored`). First, so each early
    // return below reads as the markers going.
    let mut anchored = WorldAnchored::new(egui.ctx(), "place_pins");
    // The library's edition, when it has the pins: egui paints nothing, and
    // an early return below leaves it an empty frame, which reads as the
    // markers going there too.
    let mut lib = frame.lib.as_deref_mut().filter(|l| l.lib);
    let tapped_key = lib.as_mut().and_then(|l| {
        l.lib_view = PinsView::default();
        l.lib_tapped.take()
    });
    #[cfg(not(feature = "comms"))]
    if let Some(l) = lib.as_mut() {
        l.lib_stop = None;
    }
    // Before everything else, driver mode included: while anybody can see where
    // this phone is, the screen says so. A share that is on without a sign of it
    // is the one thing this feature must never be.
    #[cfg(feature = "comms")]
    {
        let stop = lib.as_mut().and_then(|l| l.lib_stop.take());
        if !chat.sharing.is_empty() {
            let lines = sharing_lines(&chat);
            let stop = match lib.as_mut() {
                Some(l) => {
                    l.lib_view.sharing = lines;
                    stop
                }
                None => {
                    let ctx = egui.ctx_mut().clone();
                    sharing_chip(&ctx, &lines)
                }
            };
            if let Some(channel) = stop {
                chat.stop_sharing(&channel);
            }
        }
    }
    // Driver mode owns the screen and strips the map to the route — but this
    // system never learned that, so the teardrops kept drawing over the
    // navigation view, and every one of them is a live `Area` whose click is a
    // `Teleport`. Tapping a saved place at 50 km/h is not a gesture that should
    // be reachable.
    if frame.driver.active {
        return;
    }
    #[cfg(feature = "comms")]
    let chat_points = chat.world_points();
    #[cfg(feature = "comms")]
    let post_points = communities.posts.map_posts();
    #[cfg(feature = "comms")]
    let now_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    #[cfg(feature = "comms")]
    let people: Vec<crate::systems::chat::SharedLocation> = chat
        .locations
        .iter()
        .filter(|l| l.until_ms > now_ms)
        .cloned()
        .collect();
    #[cfg(feature = "comms")]
    let chat_world = chat
        .world
        .clone()
        .filter(|k| crate::systems::chat::ChatState::is_private(k));
    // Signed in with an avatar on the map: you are one of the people on it.
    #[cfg(feature = "comms")]
    let me = identity
        .as_ref()
        .and_then(|i| i.username.clone())
        .filter(|_| !local.is_empty());
    // Only the app has anything from a chat to pin (`comms`).
    #[cfg(feature = "comms")]
    let nothing_of_people = chat_points.is_empty()
        && post_points.is_empty()
        && people.is_empty()
        && chat_world.is_none()
        && contacts.whereabouts.is_empty()
        && me.is_none();
    #[cfg(not(feature = "comms"))]
    let nothing_of_people = true;
    if store.data.bookmarks.is_empty() && nothing_of_people {
        return;
    }
    let Some(anchor) = map_anchor.geo else {
        return; // no map yet; nothing to pin to
    };
    let Ok((cam, cam_cell, cam_tf)) = camera.single() else {
        return;
    };

    let map_rect = map_area(&mut egui, &frame.keyboard, &frame.route, &frame.lens);
    // Projection answers in window points; the pins are drawn in egui's. Bound
    // once here rather than called per pin, so the `ResMut` isn't re-borrowed
    // inside the loop that owns `ctx`.
    let ui_scale = egui.ui_scale();
    let to_ui = |p: Vec2| egui::pos2(p.x / ui_scale, p.y / ui_scale);
    let ctx = egui.ctx_mut().clone();
    if map_rect.width() < 2.0 * HEAD_R || map_rect.height() < 3.0 * HEAD_R {
        return; // panels have taken the screen (compact layout)
    }

    let cam_world = anchored_position_f64(cam_cell, cam_tf.translation);
    let in_space = globe.in_space(cam_cell, cam_tf);
    // The camera basis to project through, rebuilt from *this* frame's `Transform`.
    //
    // Reading the camera's `GlobalTransform` here would be a frame behind: big_space
    // propagates in `PostUpdate` (`BigSpaceSystems::PropagateHighPrecision`) while this
    // runs in `Update`, so the `GlobalTransform` in the world is still the one the
    // *previous* frame rendered with — but the map the pins sit on is drawn from this
    // frame's. The translation cancels out, the rotation does not: an orbit drag at
    // 90°/s is 1.5° of stale view per frame, tens of pixels of pins swimming over a map
    // that is standing still. Rebuilding is exact and free, because the camera is the
    // floating origin: `Grid::global_transform` subtracts the origin's own cell and
    // rotation, so for this one entity `GlobalTransform` *is* `Transform`.
    //
    // Translation stays at zero and each place is projected as a camera-relative delta
    // (`place_delta` already produces one, in f64). That keeps the f32 projection from
    // having to cancel the camera's own offset — up to a `WORLD_SWITCH_THRESHOLD` of
    // shared magnitude — back out of every point it is added to.
    let cam_view = GlobalTransform::from(Transform::from_rotation(cam_tf.rotation));

    // Where a place stands on the screen this frame: `None` behind the camera,
    // over the horizon, off the map or under the map's buttons.
    let project = |lat: f64, lng: f64| -> Option<egui::Pos2> {
        let delta = place_delta(&globe, anchor, cam_world, in_space, lat, lng, &terrain)?;
        let screen = cam.world_to_viewport(&cam_view, delta.as_vec3()).ok()?;
        let tip = to_ui(screen);
        shown(&ctx, map_rect, tip).then_some(tip)
    };
    // The library's edition centres its labels by egui's measure of them
    // (`text_width`); egui's own needs none.
    #[cfg(feature = "comms")]
    let for_lib = lib.is_some();
    // Every marker of this frame in the order they stack, the last on top,
    // and what a tap on each does. Placed once for both editions.
    let mut placed: Vec<(PinItem, PinTap)> = Vec::new();
    for (i, b) in store.data.bookmarks.iter().enumerate() {
        let Some(tip) = project(b.lat, b.lng) else {
            continue;
        };
        placed.push((
            PinItem {
                key: format!("place:{}", b.id),
                tip,
                name: b.name.clone(),
                look: PinLook::Place {
                    color: store.pin_color(b.folder),
                },
            },
            PinTap::Bookmark(i),
        ));
    }
    // What people last said, over their markers: whose, where (the top of
    // the marker), the words, how strongly, and whether they are yours.
    #[cfg(feature = "comms")]
    let mut balloons: Vec<(String, egui::Pos2, String, f32, bool)> = Vec::new();
    // Everything that comes from a chat or a community (`comms`): the web
    // build pins saved places only, and the page around it has the rest.
    #[cfg(feature = "comms")]
    {
        // A chat's points, as the design's numbered labels ([`point_pin`]) rather
        // than teardrops, so they read as "from the conversation" and never as one
        // of your own saved places. Ids from a range no bookmark reaches, since egui
        // keys each pin's `Area` by it.
        for (i, (point, label)) in chat_points.iter().enumerate() {
            let Some(tip) = project(point.latitude, point.longitude) else {
                continue;
            };
            // `world_points` numbers each label "<n>  <name>", matching the card.
            let (number, name) = label.split_once("  ").unwrap_or(("", label.as_str()));
            let width = if for_lib {
                point_head_width(&ctx, name)
            } else {
                0.0
            };
            placed.push((
                PinItem {
                    key: format!("point:{}", CHAT_PIN_IDS + i as u64),
                    tip,
                    name: name.to_string(),
                    look: PinLook::Point {
                        number: number.to_string(),
                        width,
                    },
                },
                PinTap::ChatPoint(i),
            ));
        }
        // A community's public posts, where they were written, in the colour
        // that means "public" everywhere else (`community_posts`).
        let lavender = theme::PUBLIC;
        let lavender = [
            f32::from(lavender.r()) / 255.0,
            f32::from(lavender.g()) / 255.0,
            f32::from(lavender.b()) / 255.0,
        ];
        for (i, (_, _, point, label)) in post_points.iter().enumerate() {
            let Some(tip) = project(point.latitude, point.longitude) else {
                continue;
            };
            placed.push((
                PinItem {
                    key: format!("post:{}", POST_PIN_IDS + i as u64),
                    tip,
                    name: label.clone(),
                    look: PinLook::Place { color: lavender },
                },
                PinTap::Post(i),
            ));
        }
        // The width of a person's name chip, for the library's edition.
        let chip = |name: &str| {
            if for_lib {
                chat_people::chip_width(&ctx, name)
            } else {
                0.0
            }
        };
        // People's real locations, shared with a private chat: a person's marker,
        // not a place's teardrop, in the blue that means nothing else
        // (`chat_people::MarkerKind::Phone`, `theme::REAL_LOCATION`).
        for (i, l) in people.iter().enumerate() {
            let Some(tip) = project(l.latitude, l.longitude) else {
                continue;
            };
            placed.push((
                PinItem {
                    key: format!("phone:{}", l.from.to_lowercase()),
                    tip,
                    name: l.from.clone(),
                    look: PinLook::Person {
                        kind: chat_people::MarkerKind::Phone,
                        chip: chip(&l.from),
                    },
                },
                PinTap::Phone(i),
            ));
        }
        // The people in the chat whose World view is open, at their avatars, with
        // what each last said floating over them (`chat_people`, design v5 B).
        // With no chat's view open, `whereabouts` is every contact the Friends
        // layer asked after, drawn the same way without the balloons: there is no
        // conversation for them to come from.
        if chat_world.is_some() || !contacts.whereabouts.is_empty() || me.is_some() {
            let key = chat_world.unwrap_or_default();
            let now = chat.clock;
            // Over the head on the flat map; from orbit a head is not a distance.
            let head_up = if in_space {
                DVec3::ZERO
            } else {
                DVec3::Y * chat_people::HEAD_HEIGHT_M
            };
            for w in &contacts.whereabouts {
                let (lat, lng) = w.at(now);
                let Some(ground) =
                    place_delta(&globe, anchor, cam_world, in_space, lat, lng, &terrain)
                else {
                    continue;
                };
                // Remote avatars are root entities whose translation is already the
                // absolute sim position (`chat_overlay::render_chat_bubbles`).
                let foot = remotes
                    .iter()
                    .find(|(r, _)| r.sim_id == w.avatar_id)
                    .map(|(_, tf)| tf.translation.as_dvec3() - cam_world)
                    .filter(|drawn| !in_space && chat_people::same_avatar(*drawn, ground))
                    .unwrap_or(ground);
                let Ok(screen) = cam.world_to_viewport(&cam_view, (foot + head_up).as_vec3())
                else {
                    continue;
                };
                let tip = to_ui(screen);
                if !shown(&ctx, map_rect, tip) {
                    continue;
                }
                placed.push((
                    PinItem {
                        key: format!("avatar:{}", w.username.to_lowercase()),
                        tip,
                        name: w.username.clone(),
                        look: PinLook::Person {
                            kind: chat_people::MarkerKind::Avatar,
                            chip: chip(&w.username),
                        },
                    },
                    PinTap::Person(w.username.clone()),
                ));
                if let Some((text, alpha)) = chat_people::latest_said(&chat, &key, &w.username) {
                    let over = egui::pos2(tip.x, tip.y - chat_people::marker_height());
                    balloons.push((w.username.clone(), over, text, alpha, false));
                }
            }
            // You, over your own avatar and with your own last line over that:
            // from a city's height your avatar is a speck, and the map showed
            // everybody but the person holding it. Placed from this client's own
            // avatar, which is exact; from orbit through its latitude and
            // longitude, as everybody else is.
            if let (Some(name), Ok((cell, tf))) = (&me, local.single()) {
                let abs = anchored_position_f64(cell, tf.translation);
                let (lat, lng) = crate::grid::world_to_lat_lng(anchor, abs.x, abs.z);
                let foot = if in_space {
                    place_delta(&globe, anchor, cam_world, in_space, lat, lng, &terrain)
                } else {
                    Some(abs - cam_world)
                };
                let tip = foot
                    .and_then(|foot| {
                        cam.world_to_viewport(&cam_view, (foot + head_up).as_vec3())
                            .ok()
                    })
                    .map(to_ui)
                    .filter(|tip| shown(&ctx, map_rect, *tip));
                if let Some(tip) = tip {
                    placed.push((
                        PinItem {
                            key: "me".to_string(),
                            tip,
                            name: name.clone(),
                            look: PinLook::Person {
                                kind: chat_people::MarkerKind::Avatar,
                                chip: chip(name),
                            },
                        },
                        PinTap::Jump(Teleport::geo(lat, lng, None), name.clone()),
                    ));
                    if let Some((text, alpha)) = chat_people::latest_mine(&chat, &key) {
                        let over = egui::pos2(tip.x, tip.y - chat_people::marker_height());
                        balloons.push((String::new(), over, text, alpha, true));
                    }
                }
            }
        }
    }

    // Drawn by egui, or handed to the library; a tap either way is one of the
    // markers just placed.
    let drawn = placed.len();
    let mut tapped: Option<PinTap> = None;
    match lib.as_mut() {
        Some(l) => {
            if let Some(key) = &tapped_key {
                tapped = placed
                    .iter()
                    .find(|(p, _)| &p.key == key)
                    .map(|(_, t)| t.clone());
            }
            #[cfg(feature = "comms")]
            {
                l.lib_view.balloons = balloons
                    .iter()
                    .map(|(who, over, text, alpha, mine)| {
                        chat_people::balloon_item(&ctx, map_rect, who, *over, text, *alpha, *mine)
                    })
                    .collect();
            }
            l.lib_view.pins = placed.into_iter().map(|(p, _)| p).collect();
        }
        None => {
            for (p, tap) in &placed {
                anchored.at(p.tip);
                let hit = match &p.look {
                    PinLook::Place { color } => pin(&ctx, map_rect, &p.key, p.tip, *color, &p.name),
                    #[cfg(feature = "comms")]
                    PinLook::Point { number, .. } => {
                        point_pin(&ctx, map_rect, &p.key, p.tip, number, &p.name)
                    }
                    #[cfg(feature = "comms")]
                    PinLook::Person { kind, .. } => {
                        chat_people::marker(&ctx, map_rect, p.tip, &p.name, *kind)
                    }
                };
                if hit {
                    tapped = Some(tap.clone());
                }
            }
            // After every marker, so a balloon is never under somebody's disc.
            #[cfg(feature = "comms")]
            for (who, over, text, alpha, mine) in &balloons {
                chat_people::balloon(&ctx, map_rect, who, *over, text, *alpha, *mine);
            }
        }
    }

    // What the tap asked for, the same for both editions.
    //
    // A jump carries the place's name alongside the teleport: the address
    // bar's recents list reads `pending_label`, and a jump that arrives
    // without one shows up there as a bare coordinate pair rather than as the
    // place you saved.
    let mut jump: Option<(Teleport, String)> = None;
    #[cfg_attr(not(feature = "comms"), allow(unused_mut))]
    let mut card: Option<crate::systems::place_card::CardPlace> = None;
    #[cfg(feature = "comms")]
    let mut person_card: Option<String> = None;
    match tapped {
        Some(PinTap::Bookmark(i)) => {
            if let Some(b) = store.data.bookmarks.get(i) {
                // Same jump the panel's row does: keep the current camera mode
                // and move it to the spot (`pose: None`).
                jump = Some((
                    Teleport {
                        lat: b.lat,
                        lng: b.lng,
                        altitude: Some(b.altitude),
                        pose: None,
                        silent: false,
                        look: false,
                    },
                    b.name.clone(),
                ));
            }
        }
        #[cfg(feature = "comms")]
        Some(PinTap::Jump(tp, name)) => jump = Some((tp, name)),
        #[cfg(feature = "comms")]
        Some(PinTap::ChatPoint(i)) => {
            // Bridge ①: the place's card, saying it is the chat's, with the
            // way back into the chat (`bridges`).
            if let (Some((point, label)), Some(key)) = (chat_points.get(i), chat.world.clone()) {
                let name = label.split_once("  ").map_or(label.as_str(), |(_, n)| n);
                let chat_name = chat.channels.iter().find(|c| c.key == key).map_or_else(
                    || key.clone(),
                    |c| crate::systems::chats::display_name(&key, &c.label),
                );
                card = Some(marker_card(
                    point.name.clone().unwrap_or_else(|| name.to_string()),
                    point.latitude,
                    point.longitude,
                    format!("{chat_name} · place"),
                    crate::systems::bridges::OpenIn::Chat(key),
                    None,
                ));
            }
        }
        #[cfg(feature = "comms")]
        Some(PinTap::Post(i)) => {
            // Bridge ①: the post's card over the map, saying whose it is,
            // with Open in Community to read it there (`bridges`).
            if let Some((community, post, point, label)) = post_points.get(i) {
                let from = communities
                    .get(*community)
                    .map_or_else(|| "Community".to_string(), |c| c.settings.name.clone());
                card = Some(marker_card(
                    label.clone(),
                    point.latitude,
                    point.longitude,
                    format!("{from} · post"),
                    crate::systems::bridges::OpenIn::Post {
                        community: *community,
                        post: *post,
                    },
                    None,
                ));
            }
        }
        #[cfg(feature = "comms")]
        Some(PinTap::Phone(i)) => {
            // Bridge ①: "sanne · sharing location", Open chat, and a route
            // that follows them. The name is the title; the line over it
            // says what kind of thing it is, and for how long (it said the
            // name twice).
            if let Some(l) = people.get(i) {
                let mins = l.until_ms.saturating_sub(now_ms) / 60_000 + 1;
                card = Some(marker_card(
                    l.from.clone(),
                    l.latitude,
                    l.longitude,
                    format!("Sharing location · {mins} min left"),
                    crate::systems::bridges::OpenIn::Chat(l.channel.clone()),
                    Some((l.channel.clone(), l.from.clone())),
                ));
            }
        }
        // Bridge ①, for a person: their card (`contacts`), whose Message is
        // the way into the chat.
        #[cfg(feature = "comms")]
        Some(PinTap::Person(name)) => person_card = Some(name),
        None => {}
    }
    // "The pins aren't showing" has three very different causes — no anchor, all
    // projected off-map, or drawn-but-invisible — and only this tells them apart.
    tracing::debug!(
        places = store.data.bookmarks.len(),
        drawn,
        in_space,
        map_rect = ?(map_rect.min.x, map_rect.min.y, map_rect.max.x, map_rect.max.y),
        "place pins"
    );
    if let Some((tp, label)) = jump {
        nav.pending_teleport = Some(tp);
        nav.pending_label = Some(label);
    }
    if card.is_some() {
        nav.pending_card = card;
    }
    #[cfg(feature = "comms")]
    if let Some(name) = person_card {
        contacts.open_card_for_name(&name, None);
    }
}

/// A Community marker's place card (bridge ①, `bridges`).
#[cfg(feature = "comms")]
fn marker_card(
    name: String,
    lat: f64,
    lng: f64,
    line: String,
    open: crate::systems::bridges::OpenIn,
    person: Option<(String, String)>,
) -> crate::systems::place_card::CardPlace {
    crate::systems::place_card::CardPlace {
        name,
        kind: None,
        context: String::new(),
        lat,
        lng,
        from: Some(crate::systems::bridges::Origin { line, open, person }),
    }
}

/// The part of the viewport a world-anchored overlay may paint on: the free
/// central area, minus every floating `Area` that reserves nothing in the root
/// `Ui` and would otherwise be painted over.
///
/// Shared with [`crate::systems::locate`]'s device arrow, which has the same
/// problem and had no business growing a second copy of this list — the list is
/// the part that goes out of date, one forgotten overlay at a time.
///
/// **The keyboard, the dock and the route sheet are all here for one reason.**
/// They are floating `Area`s, so `available_rect_before_wrap` cannot know about
/// them, and this system runs last at the same `Order::Middle` the dock draws in
/// — so within that order a pin raised later wins egui's hit test. On a phone the
/// dock is ~240 pt of a 274 pt viewport, so a saved place projecting anywhere
/// near the bottom edge was swallowing taps meant for Search and the app menu,
/// the two controls the whole dock layout exists to put under a thumb. Only the
/// keyboard ever got this treatment; the other two edges were simply never added.
///
/// Read off the shared root `Ui` *before* the caller borrows the context, so the
/// two borrows of `egui` don't overlap (the context is a cheap `Arc` clone).
/// The map's round buttons and their columns (`dock::render_dock`), as they
/// were drawn this frame: a world-anchored overlay culls itself against
/// them as against the edges in [`map_area`], or a marker paints over a
/// button and takes its tap (2026-11-11, a friend's marker over the right
/// column in Earth mode). Written by the dock, which runs earlier in the
/// panel chain than every overlay that reads it.
pub(crate) fn set_chrome_holes(ctx: &egui::Context, holes: Vec<egui::Rect>) {
    ctx.data_mut(|d| d.insert_temp(chrome_holes_id(), holes));
}

fn chrome_holes_id() -> egui::Id {
    egui::Id::new("map_chrome_holes")
}

/// Whether an overlay anchored at `tip` stays clear of the map's buttons. A
/// pin or a marker hangs *above* its tip, by up to [`PIN_REACH`], so a tip
/// that far under a column still draws on it.
pub(crate) fn clear_of_chrome(ctx: &egui::Context, tip: egui::Pos2) -> bool {
    let holes: Vec<egui::Rect> = ctx
        .data(|d| d.get_temp(chrome_holes_id()))
        .unwrap_or_default();
    !holes.iter().any(|h| {
        egui::Rect::from_min_max(
            h.min - egui::vec2(PIN_SIDE, 0.0),
            h.max + egui::vec2(PIN_SIDE, PIN_REACH),
        )
        .contains(tip)
    })
}

/// How far above its tip a pin or a person's marker reaches, and how far to
/// either side: a head and its stem.
const PIN_REACH: f32 = 56.0;
const PIN_SIDE: f32 = 22.0;

/// Inside the map, and clear of its buttons.
fn shown(ctx: &egui::Context, map_rect: egui::Rect, tip: egui::Pos2) -> bool {
    map_rect.contains(tip) && clear_of_chrome(ctx, tip)
}

pub(crate) fn map_area(
    egui: &mut EguiManager,
    keyboard: &crate::systems::onscreen_keyboard::OnScreenKeyboard,
    route: &crate::systems::navigation::RouteState,
    lens: &crate::systems::scenario::LensSlot,
) -> egui::Rect {
    let mut map_rect = egui.ui_mut().available_rect_before_wrap();
    if let Some(slab) = keyboard.slab() {
        if slab.top() > map_rect.top() {
            map_rect.max.y = map_rect.max.y.min(slab.top());
        } else {
            map_rect.min.y = map_rect.min.y.max(slab.bottom());
        }
    }
    map_rect.max.y -= crate::systems::dock::reserved_bottom();
    map_rect.min.y += crate::systems::dock::reserved_top();
    if let Some(sheet) = route.sheet_rect {
        map_rect.max.y = map_rect.max.y.min(sheet.min.y);
    }
    // The lens, last frame: a sheet along the bottom, or a column on the left
    // (`scenario::LensSlot`, in UI points).
    match lens.0 {
        Some((lens, true)) => map_rect.max.y = map_rect.max.y.min(lens.min.y),
        Some((lens, false)) => map_rect.min.x = map_rect.min.x.max(lens.max.x),
        None => {}
    }
    map_rect
}

/// Where a place is relative to the camera, in metres, or `None` when it should
/// not be drawn at all (over the horizon in space, or out of range on the flat
/// frame).
///
/// `pub(crate)` because the device arrow (`systems::locate`) is the same question
/// asked about the phone's own position: which tier the map is in decides whether
/// a coordinate lives on the anchored Mercator plane or on the sphere, and that
/// decision must not be made twice.
pub(crate) fn place_delta(
    globe: &GlobeState,
    anchor: (f64, f64, u32),
    cam_world: DVec3,
    in_space: bool,
    lat: f64,
    lng: f64,
    terrain: &crate::systems::terrain::TerrainField,
) -> Option<DVec3> {
    if in_space {
        // On the sphere itself. The point is visible iff the camera is on its
        // side of the tangent plane — the standard horizon test, which is what
        // keeps pins from showing through the planet.
        let centre = globe.center_local.as_dvec3();
        let normal = globe.direction_local(lat, lng).as_dvec3();
        let point = centre + normal * globe.radius as f64;
        ((cam_world - point).dot(normal) > 0.0).then_some(point - cam_world)
    } else {
        let (wx, wz) = lat_lng_to_world(anchor, lat, lng);
        // Ground level: `y = 0` is the flat map's surface, and the terrain field is
        // how far the measured ground stands off it (0 with the layer off). A pin
        // left at 0 over relief hangs in the air above its own hillside, or sinks
        // into it — and the pin is *pointing at* a place, so where its tip lands is
        // the whole feature.
        //
        // And the Earth's curve under it, as the basemap has (`GlobeState::drop_at`).
        // Towns are named out to `MAX_FLAT_RANGE_M`, where the flat plane stands
        // 4.9 km over the ground actually drawn: from 10 km up such a name was
        // painted in the sky above the horizon.
        let (fx, fz) = (wx as f32, wz as f32);
        let y = terrain.ground_at(fx, fz) - globe.drop_at(fx.hypot(fz));
        let point = DVec3::new(wx, f64::from(y), wz);
        let delta = point - cam_world;
        // Past the horizon, as in space: the camera on the far side of the
        // sphere's tangent plane at the place. Tested against the sphere, not the
        // measured ground, and with a margin, because at eye level in a polder the
        // camera is below sea level and a dike beside it stands above the camera.
        const HORIZON_MARGIN_M: f64 = 200.0;
        let normal = globe.surface_normal(fx, fz).as_dvec3();
        let sea = DVec3::new(wx, f64::from(-globe.drop_at(fx.hypot(fz))), wz);
        let over = (cam_world - sea).dot(normal) > -HORIZON_MARGIN_M;
        (over && delta.x.hypot(delta.z) < MAX_FLAT_RANGE_M).then_some(delta)
    }
}

/// Paint one pin with its tip at `tip` and return whether it was clicked.
///
/// Its own `Area` per pin, so egui owns the hover/click and reports the pointer
/// as taken — the world-space click handlers (POI info cards, route endpoint
/// picking, prim selection) then correctly leave a click on a pin alone.
fn pin(
    ctx: &egui::Context,
    map_rect: egui::Rect,
    id: &str,
    tip: egui::Pos2,
    color: [f32; 3],
    name: &str,
) -> bool {
    let hue = egui::Color32::from_rgb(
        (color[0] * 255.0) as u8,
        (color[1] * 255.0) as u8,
        (color[2] * 255.0) as u8,
    );
    // Rim: the same hue darkened. A white outline would read as a sticker and a
    // black one muddies every colour; darkening keeps the pin one object.
    let rim = egui::Color32::from_rgb(
        (color[0] * 145.0) as u8,
        (color[1] * 145.0) as u8,
        (color[2] * 145.0) as u8,
    );

    // Keyed by the place's id, not its position: a per-frame id would leave egui
    // remembering one dead `Area` per pin per frame for the whole session.
    let response = egui::Area::new(egui::Id::new(("place_pin", id)))
        .fixed_pos(tip)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            // Never paint over a panel, even when the tip is just inside the map.
            ui.set_clip_rect(map_rect);
            let (_rect, response) = ui.allocate_exact_size(
                egui::vec2(2.0 * HEAD_R, (HEAD_LIFT + 1.0) * HEAD_R),
                egui::Sense::click(),
            );
            let response = theme::named(ui, response, name);
            let scale = if response.hovered() { HOVER_SCALE } else { 1.0 };
            let r = HEAD_R * scale;
            // Paint from the *exact* projected tip, not the allocated rect's bottom
            // centre. egui rounds every `Area`'s position to a whole physical pixel
            // (`containers::area::round_area_position`, so dragged windows don't have
            // their widgets shimmer against each other), which is right for chrome and
            // wrong for a marker pinned to the ground: the map slides smoothly under a
            // pin that can only step a pixel at a time, and the pin reads as wobbling
            // on its spot. The rect still senses the click — half a pixel of offset
            // between the hit area and the silhouette is not something you can aim at.
            let centre = egui::pos2(tip.x, tip.y - HEAD_LIFT * r);
            // A pin seen for the first time pops about its head (`motion::pop`);
            // one that merely scrolls back into view does not.
            let pres =
                crate::systems::motion::presence(ui.ctx(), egui::Id::new(("place_pin", id)), true);
            crate::systems::motion::pop(ui, pres, centre);
            let painter = ui.painter();

            // A soft contact shadow at the tip, so the pin reads as standing on
            // the map rather than floating in the middle of it.
            painter.circle_filled(tip, r * 0.30, egui::Color32::from_black_alpha(45));

            // The teardrop: the head circle plus the two tangent lines down to
            // the tip. That outline is convex, so one antialiased polygon draws
            // the whole silhouette — fill and rim in a single shape, with no
            // seam where a separate triangle would meet the circle.
            painter.add(egui::Shape::convex_polygon(
                teardrop(tip, centre, r),
                hue,
                egui::Stroke::new(1.0, rim),
            ));
            // The hole. Semi-transparent white so it reads on any basemap.
            painter.circle_filled(
                centre,
                r * HOLE_R,
                egui::Color32::from_rgba_unmultiplied(
                    theme::DARK32.cream.r(),
                    theme::DARK32.cream.g(),
                    theme::DARK32.cream.b(),
                    235,
                ),
            );

            if response.hovered() {
                label(ui, map_rect, centre, r, name);
            }
            response
        })
        .inner;

    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// The width of [`point_pin`]'s label for `name`: the number's disc, the
/// name (cut at 170 pt) and the label's edges.
#[cfg(feature = "comms")]
fn point_head_width(ctx: &egui::Context, name: &str) -> f32 {
    4.0 + 22.0 + 7.0 + text_width(ctx, name, egui::FontId::proportional(12.5)).min(170.0) + 10.0
}

/// A chat's point on its World view: the design's `.ppin` — a cream label with
/// the point's number in an ink circle and its name, on a thin stem down to a
/// dot on the ground. Always labelled: a conversation has a handful of places,
/// and the number is what ties each to its card in the thread.
fn point_pin(
    ctx: &egui::Context,
    map_rect: egui::Rect,
    id: &str,
    tip: egui::Pos2,
    number: &str,
    name: &str,
) -> bool {
    const STEM: f32 = 16.0;
    const DOT_R: f32 = 4.0;
    const HEAD_H: f32 = 30.0;
    let font = egui::FontId::proportional(12.5);
    let response = egui::Area::new(egui::Id::new(("chat_point_pin", id)))
        .fixed_pos(tip)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.set_clip_rect(map_rect);
            let pres = crate::systems::motion::presence(
                ui.ctx(),
                egui::Id::new(("chat_point_pin", id)),
                true,
            );
            crate::systems::motion::pop(ui, pres, tip);
            let p = ui.painter();
            let mut job = egui::text::LayoutJob::simple_singleline(
                name.to_string(),
                font.clone(),
                theme::DARK32.ink,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(170.0);
            let galley = p.layout_job(job);
            let head_w = 4.0 + 22.0 + 7.0 + galley.size().x + 10.0;
            let (_rect, response) = ui.allocate_exact_size(
                egui::vec2(head_w, HEAD_H + STEM + DOT_R * 2.0),
                egui::Sense::click(),
            );
            let response = theme::named(ui, response, name);
            // Painted from the exact tip, as [`pin`] is, so it does not step a
            // whole pixel at a time over a map that is sliding smoothly.
            let p = ui.painter();
            let dot = egui::pos2(tip.x, tip.y - DOT_R);
            let stem_top = dot.y - DOT_R - STEM;
            let head = egui::Rect::from_min_size(
                egui::pos2(tip.x - head_w / 2.0, stem_top - HEAD_H),
                egui::vec2(head_w, HEAD_H),
            );
            p.add(
                egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: 16,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(90),
                }
                .as_shape(head, egui::CornerRadius::same(14)),
            );
            p.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(tip.x - 1.0, stem_top),
                    egui::pos2(tip.x + 1.0, dot.y),
                ),
                egui::CornerRadius::ZERO,
                theme::DARK32.cream,
            );
            p.circle(
                dot,
                DOT_R,
                theme::DARK32.cream,
                egui::Stroke::new(2.0, theme::DARK32.ink),
            );
            let fill = if response.hovered() {
                egui::Color32::WHITE
            } else {
                theme::DARK32.cream
            };
            p.rect_filled(head, egui::CornerRadius::same(14), fill);
            let n = egui::pos2(head.left() + 4.0 + 11.0, head.center().y);
            p.circle_filled(n, 11.0, theme::DARK32.ink);
            p.text(
                n,
                egui::Align2::CENTER_CENTER,
                number,
                egui::FontId::proportional(12.0),
                theme::DARK32.cream,
            );
            let gs = galley.size();
            p.galley(
                egui::pos2(n.x + 11.0 + 7.0, head.center().y - gs.y / 2.0),
                galley,
                theme::DARK32.ink,
            );
            response
        })
        .inner;
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Paint the pin's silhouette to fit `rect`, in the folder colour — the same
/// marker [`pin`] puts on the map, at whatever size the caller has room for.
///
/// The Places panel's rows use it for a place with no photo. That slot used to be
/// a `first_drawable(&["📍", "⌖", "•"])` text glyph, which is a probe for
/// *whatever the bundled font happens to draw*: it resolved to `⌖`, a crosshair
/// with no relationship to the teardrop standing over the same place on the map.
/// A painted pin cannot fall back to the wrong picture, and the list and the map
/// then show one marker rather than two unrelated ones.
pub fn paint_pin_glyph(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    // Fit the whole silhouette — `2r` wide by `(HEAD_LIFT + 1) r` tall — inside
    // the slot, then stand it on the bottom of the fitted box.
    let r = (rect.width() * 0.5).min(rect.height() / (HEAD_LIFT + 1.0));
    let tip = egui::pos2(
        rect.center().x,
        rect.center().y + (HEAD_LIFT + 1.0) * r * 0.5,
    );
    let centre = egui::pos2(tip.x, tip.y - HEAD_LIFT * r);
    let rim = egui::Color32::from_rgb(
        (f32::from(color.r()) * 0.57) as u8,
        (f32::from(color.g()) * 0.57) as u8,
        (f32::from(color.b()) * 0.57) as u8,
    );
    painter.add(egui::Shape::convex_polygon(
        teardrop(tip, centre, r),
        color,
        egui::Stroke::new(1.0, rim),
    ));
    painter.circle_filled(centre, r * HOLE_R, theme::DARK32.cream);
}

/// Outline of the pin: the tip, then the head circle's arc from one tangent
/// point round the top to the other. With the centre `HEAD_LIFT × r` above the
/// tip the tangent points sit `acos(1 / HEAD_LIFT)` either side of straight
/// down, so the body meets the head smoothly instead of cutting a notch in it.
fn teardrop(tip: egui::Pos2, centre: egui::Pos2, r: f32) -> Vec<egui::Pos2> {
    // egui's y grows downward, so "straight down from the centre" is +90°.
    let half = (1.0f32 / HEAD_LIFT).acos();
    let (from, to) = (
        std::f32::consts::FRAC_PI_2 + half,
        std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU - half,
    );
    const SEGMENTS: usize = 24;
    let mut points = Vec::with_capacity(SEGMENTS + 2);
    points.push(tip);
    for i in 0..=SEGMENTS {
        let a = from + (to - from) * i as f32 / SEGMENTS as f32;
        points.push(egui::pos2(centre.x + r * a.cos(), centre.y + r * a.sin()));
    }
    points
}

/// The place's name in a chip above the pin, on hover. Kept off the map
/// otherwise: a saved-places layer with every name always drawn covers the city
/// it is describing, and the panel is where the list lives.
fn label(ui: &egui::Ui, map_rect: egui::Rect, centre: egui::Pos2, r: f32, name: &str) {
    let font = egui::FontId::proportional(theme::SIZE_SUB);
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_string(), font, theme::DARK32.cream);
    let pad = theme::CHIP_PADDING;
    let mut chip = egui::Rect::from_center_size(
        egui::pos2(centre.x, centre.y - r - galley.size().y * 0.5 - 8.0),
        galley.size() + pad * 2.0,
    );
    // Keep it on the map when the pin is near an edge.
    chip = chip.translate(egui::vec2(
        (map_rect.left() + 2.0 - chip.left()).max(0.0)
            + (map_rect.right() - 2.0 - chip.right()).min(0.0),
        (map_rect.top() + 2.0 - chip.top()).max(0.0),
    ));
    let painter = ui.painter();
    painter.rect_filled(chip, theme::RADIUS_SM, theme::LABEL_CHIP);
    painter.galley(chip.min + pad, galley, theme::DARK32.cream);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
    use big_space::prelude::Grid;

    /// The silhouette has to be convex — `Shape::convex_polygon` renders a
    /// concave outline with folded-over antialiasing, and the whole pin is one
    /// such polygon. Walk the outline and assert every turn goes the same way.
    #[test]
    fn the_pin_outline_is_convex() {
        let tip = egui::pos2(100.0, 200.0);
        let r = HEAD_R;
        let centre = egui::pos2(tip.x, tip.y - HEAD_LIFT * r);
        let p = teardrop(tip, centre, r);
        assert!(p.len() > 8);
        let cross = |a: egui::Pos2, b: egui::Pos2, c: egui::Pos2| {
            (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x)
        };
        let sign = (1..p.len() - 1)
            .map(|i| cross(p[i - 1], p[i], p[i + 1]))
            .find(|v| v.abs() > 1e-4)
            .expect("the outline has to turn somewhere")
            .signum();
        for i in 1..p.len() - 1 {
            let turn = cross(p[i - 1], p[i], p[i + 1]);
            assert!(
                turn.abs() < 1e-4 || turn.signum() == sign,
                "outline turns back on itself at vertex {i} ({turn})"
            );
        }
        // And the closing edge, back from the last arc point to the tip.
        let last = p.len() - 1;
        for (a, b, c) in [(p[last - 1], p[last], p[0]), (p[last], p[0], p[1])] {
            let turn = cross(a, b, c);
            assert!(
                turn.abs() < 1e-4 || turn.signum() == sign,
                "closing edge is concave"
            );
        }
    }

    /// The tip must be the pin's lowest point and the head must sit above it —
    /// the pin points *at* the place, and an inverted or offset silhouette would
    /// mark the wrong spot on the map by its own height.
    #[test]
    fn the_pin_points_at_its_place() {
        let tip = egui::pos2(50.0, 80.0);
        let r = HEAD_R;
        let centre = egui::pos2(tip.x, tip.y - HEAD_LIFT * r);
        let p = teardrop(tip, centre, r);
        assert!(
            p.iter().skip(1).all(|q| q.y < tip.y),
            "the tip is the bottom of the pin"
        );
        let top = p.iter().map(|q| q.y).fold(f32::INFINITY, f32::min);
        assert!(
            ((tip.y - top) - (HEAD_LIFT + 1.0) * r).abs() < 0.01,
            "pin height must match the space the widget allocates"
        );
    }

    /// Pins are projected through a camera basis rebuilt from the camera's
    /// `Transform`, because the propagated `GlobalTransform` is a frame stale in
    /// `Update` (see the module docs). That substitution is only exact while
    /// big_space keeps giving the floating origin itself an unrotated,
    /// unoffset grid — `LocalFloatingOrigin::compute_all` resets the grid
    /// *containing* the origin to `(ZERO, IDENTITY)`, which is the default state
    /// a fresh `Grid` is in. If a big_space re-sync ever changes that, the rebuilt
    /// basis silently stops matching the one the frame is rendered with and the
    /// wobble comes back looking like a rendering bug.
    #[test]
    fn the_floating_origin_camera_is_its_own_global_transform() {
        let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
        let tf = Transform::from_xyz(12.0, -34.0, 56.0).with_rotation(Quat::from_euler(
            EulerRot::YXZ,
            0.7,
            -0.4,
            0.2,
        ));
        // The camera's own cell: `compute_all` seeds the origin's grid with it, so
        // the cell offset is zero whatever the cell happens to be.
        let propagated = grid.global_transform(&GridCell::default(), &tf);
        let rebuilt = GlobalTransform::from(tf);
        assert!(
            propagated.affine().abs_diff_eq(rebuilt.affine(), 1e-5),
            "rebuilding the camera basis from its Transform must match propagation:\n\
             propagated {:?}\nrebuilt    {:?}",
            propagated.affine(),
            rebuilt.affine(),
        );
    }

    /// In space, a place on the far side of the planet must not draw — the
    /// projection happily puts it on screen, straight through the globe.
    #[test]
    fn pins_behind_the_planet_are_dropped() {
        let mut globe = GlobeState::default();
        globe.radius = 6_378_137.0;
        globe.anchor_lat = 0.0;
        globe.anchor_lng = 0.0;
        globe.center_local = Vec3::new(0.0, -globe.radius, 0.0);
        globe.ready = true;

        // Camera 1,000 km over the anchor.
        let up = globe.direction_local(0.0, 0.0).as_dvec3();
        let cam = globe.center_local.as_dvec3() + up * (globe.radius as f64 + 1_000_000.0);
        let anchor = (0.0, 0.0, 14);

        assert!(
            place_delta(&globe, anchor, cam, true, 0.0, 0.0, &Default::default()).is_some(),
            "the point straight below the camera is visible"
        );
        assert!(
            place_delta(&globe, anchor, cam, true, 0.0, 180.0, &Default::default()).is_none(),
            "the antipode is behind the planet"
        );
    }
}
