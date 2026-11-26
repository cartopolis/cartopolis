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
//! **Where it may be drawn.** Pins are culled against the map: the viewport
//! less the side panels, the keyboard, the dock's bars, the route sheet and
//! the lens ([`map_area`]), and clear of the map's round buttons
//! ([`clear_of_chrome`]). Without that, a pin behind the Places panel would
//! stand on the list it belongs to, and one over a button would take its tap.
//!
//! **Staying put while the camera moves.** A pin is projected every frame, so it
//! is only as steady as its projection. Projecting through the camera's
//! `GlobalTransform` uses last frame's orientation (big_space propagates in
//! `PostUpdate`; this runs in `Update`), so the camera basis is rebuilt from
//! this frame's `Transform`. See `docs/notes/screen-overlay-projection.md`.
//!
//! Below the space tier a place is placed through the anchored Mercator frame
//! (`crate::grid::lat_lng_to_world`) — the same frame the tiles are laid in, so the
//! pin sits on its building. In space that frame is meaningless, so the point is
//! taken on the globe itself (`globe::GlobeState::direction_local`) and pins on
//! the far side are dropped by the horizon test.
//!
//! **Placed here, drawn by the library.** Where every marker goes, which are
//! culled and what a tap on one does are worked out here; the component
//! library draws them (`systems::lib_pins`) from the list this system
//! publishes in [`PlacePins::lib_view`], and its taps come back through
//! [`PlacePins::lib_tapped`].

use crate::components::FreeCamera;
#[cfg(feature = "comms")]
use crate::components::{Avatar, RemoteAvatar};
use crate::resources::MapAnchor;
#[cfg(feature = "comms")]
use crate::systems::chat_people;
use bevy::ecs::system::SystemParam;
use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::grid::anchored_position_f64;
use crate::grid::lat_lng_to_world;
use crate::systems::geo_nav::{GeoNavState, Teleport};
use crate::systems::globe::GlobeState;
#[cfg(feature = "comms")]
use crate::systems::theme;
use crate::systems::ui_input::{UiInput, UiMetrics};
use crate::systems::user_store::UserStore;

/// The least map, points, worth placing pins on: under it the panels have
/// taken the screen (the compact layout). A pin's head across and its
/// height.
const MIN_MAP_W: f32 = 15.0;
const MIN_MAP_H: f32 = 22.5;
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
/// The sharing chip's lines, one per chat this phone's location goes to: the
/// chat's key, and "◎ sanne sees where your phone is · 12 min". The library's
/// chip says these words (`lib_pins`).
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

/// The pins' own state: what goes back and forth with the component
/// library, which draws them (`systems::lib_pins`). Inserted by the
/// library's system the first frame.
#[derive(Resource, Default)]
pub struct PlacePins {
    /// What the library draws this frame, written by [`place_map_pins`].
    pub lib_view: PinsView,
    /// The key of the marker the library's tap landed on ([`PinItem::key`]),
    /// done next frame by [`place_map_pins`].
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
    /// The tip, in UI points (the library's px natively).
    pub tip: Vec2,
    pub name: String,
    pub look: PinLook,
}

/// What a marker looks like: everything about it that does not move.
#[derive(Clone, Debug, PartialEq)]
pub enum PinLook {
    /// A saved place's teardrop, or a community post's, in its colour.
    Place { color: [f32; 3] },
    /// A chat's point: its number, and the label's width ([`text_size`]).
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

/// The face a label over the map is set in, for [`text_size`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "comms"), allow(dead_code))]
pub(crate) enum Face {
    /// The body face (`theme::BODY_TTF`): a chat point's name.
    Body,
    /// The brand face (`theme::BRAND_TTF`): the place names.
    Brand,
}

/// The size one line of `text` takes at `size` points in `face`, with
/// `spacing` points added after every character: what the library centres
/// a label of unknown width by (its root has no box to centre in, and a wide
/// box would take the taps meant for what is under it), and what the place
/// names make room by. Measured with `fontdue` over the face the library
/// sets the text in; no kerning, near enough to place by.
pub(crate) fn text_size(text: &str, size: f32, face: Face, spacing: f32) -> Vec2 {
    static BODY: std::sync::OnceLock<Option<fontdue::Font>> = std::sync::OnceLock::new();
    static BRAND: std::sync::OnceLock<Option<fontdue::Font>> = std::sync::OnceLock::new();
    let (cell, bytes) = match face {
        Face::Body => (&BODY, crate::systems::theme::BODY_TTF),
        Face::Brand => (&BRAND, crate::systems::theme::BRAND_TTF),
    };
    let font = cell
        .get_or_init(|| fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).ok())
        .as_ref();
    let height = font
        .and_then(|f| f.horizontal_line_metrics(size))
        .map_or(size * 1.3, |m| m.new_line_size);
    let width: f32 = text
        .chars()
        .map(|c| font.map_or(size * 0.5, |f| f.metrics(c, size).advance_width) + spacing)
        .sum();
    Vec2::new(width, height)
}

/// The frame around the pins, bundled under Bevy's parameter cap.
#[derive(SystemParam)]
pub struct PinsFrame<'w> {
    keyboard: Res<'w, crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    lens: Res<'w, crate::systems::scenario::LensSlot>,
    driver: Res<'w, crate::systems::driver::DriverState>,
    route: Res<'w, crate::systems::navigation::RouteState>,
    pins: Option<ResMut<'w, PlacePins>>,
    /// The map's buttons, which the pins keep off ([`clear_of_chrome`]).
    chrome: Res<'w, crate::systems::dock::MapChrome>,
    /// Where the side panels left the map, and the UI's scale ([`map_area`]).
    input: Res<'w, UiInput>,
    metrics: Res<'w, UiMetrics>,
}

/// Beyond this ground distance a pin is dropped below the space tier. The
/// anchored Mercator frame distorts with distance from the anchor, and a pin
/// three provinces away is projected onto the horizon haze where it means
/// nothing — it is the globe's job to show those.
const MAX_FLAT_RANGE_M: f64 = 250_000.0;

/// Place a pin per saved place (and the chat's points, a community's posts
/// and people) for the library, and do what a tap on one asked for.
///
/// Runs after the dock has written the map's buttons and after the camera
/// has moved, so pins sit on the frame the user is actually looking at
/// rather than the previous one.
pub fn place_map_pins(
    store: Res<UserStore>,
    mut nav: ResMut<GeoNavState>,
    globe: Res<GlobeState>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    map_anchor: Res<MapAnchor>,
    // The keyboard, the driver bar, the route sheet, the map's rect, and the
    // library's side of the pins ([`PlacePins`]).
    mut frame: PinsFrame,
    // The points of the private chat whose World view is on (the chat panel's
    // "Map" toggle). Placed by the same code as saved places so they share the
    // projection and the culling.
    //
    // Also where other people's shared real locations are pinned, and what
    // the "you are sharing your location" chip says ([`sharing_lines`]).
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
    // The library's side, inserted by `lib_pins` the first frame. Cleared
    // first, so each early return below leaves an empty frame, which reads as
    // the markers going.
    let Some(lib) = frame.pins.as_deref_mut() else {
        return;
    };
    lib.lib_view = PinsView::default();
    let tapped_key = lib.lib_tapped.take();
    #[cfg(not(feature = "comms"))]
    {
        lib.lib_stop = None;
    }
    // Before everything else, driver mode included: while anybody can see where
    // this phone is, the screen says so. A share that is on without a sign of it
    // is the one thing this feature must never be.
    #[cfg(feature = "comms")]
    {
        let stop = lib.lib_stop.take();
        if !chat.sharing.is_empty() {
            lib.lib_view.sharing = sharing_lines(&chat);
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

    let map_rect = map_area(
        &frame.input,
        &frame.metrics,
        &frame.keyboard,
        &frame.route,
        &frame.lens,
    );
    let holes = frame.chrome.holes.clone();
    // Projection answers in window points; the pins are placed in UI points.
    let metrics = *frame.metrics;
    let to_ui = |p: Vec2| metrics.window_to_ui(p);
    if map_rect.width() < MIN_MAP_W || map_rect.height() < MIN_MAP_H {
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
    let project = |lat: f64, lng: f64| -> Option<Vec2> {
        let delta = place_delta(&globe, anchor, cam_world, in_space, lat, lng, &terrain)?;
        let screen = cam.world_to_viewport(&cam_view, delta.as_vec3()).ok()?;
        let tip = to_ui(screen);
        shown(&holes, map_rect, tip).then_some(tip)
    };
    // Every marker of this frame in the order they stack, the last on top,
    // and what a tap on each does.
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
    let mut balloons: Vec<(String, Vec2, String, f32, bool)> = Vec::new();
    // Everything that comes from a chat or a community (`comms`): the web
    // build pins saved places only, and the page around it has the rest.
    #[cfg(feature = "comms")]
    {
        // A chat's points, as the design's numbered labels (`lib_pins::point`)
        // rather than teardrops, so they read as "from the conversation" and
        // never as one of your own saved places. Ids from a range no bookmark
        // reaches, since the library keys each marker by it.
        for (i, (point, label)) in chat_points.iter().enumerate() {
            let Some(tip) = project(point.latitude, point.longitude) else {
                continue;
            };
            // `world_points` numbers each label "<n>  <name>", matching the card.
            let (number, name) = label.split_once("  ").unwrap_or(("", label.as_str()));
            let width = point_head_width(name);
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
        // The width of a person's name chip, which the library centres by.
        let chip = |name: &str| {
            chat_people::chip_width(
                crate::systems::chat_overlay::text_size(
                    name,
                    chat_people::NAME_TEXT,
                    f32::INFINITY,
                )
                .x,
            )
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
                if !shown(&holes, map_rect, tip) {
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
                    let over = Vec2::new(tip.x, tip.y - chat_people::marker_height());
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
                    .filter(|tip| shown(&holes, map_rect, *tip));
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
                        let over = Vec2::new(tip.x, tip.y - chat_people::marker_height());
                        balloons.push((String::new(), over, text, alpha, true));
                    }
                }
            }
        }
    }

    // Handed to the library; a tap there is one of the markers just placed.
    let drawn = placed.len();
    let tapped: Option<PinTap> = tapped_key.as_ref().and_then(|key| {
        placed
            .iter()
            .find(|(p, _)| &p.key == key)
            .map(|(_, t)| t.clone())
    });
    #[cfg(feature = "comms")]
    {
        lib.lib_view.balloons = balloons
            .iter()
            .map(|(who, over, text, alpha, mine)| {
                let words = crate::systems::chat_overlay::text_size(
                    text,
                    chat_people::BALLOON_TEXT,
                    chat_people::BALLOON_WRAP,
                );
                chat_people::balloon_item(words, map_rect, who, *over, text, *alpha, *mine)
            })
            .collect();
    }
    lib.lib_view.pins = placed.into_iter().map(|(p, _)| p).collect();

    // What the tap asked for.
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

/// Whether an overlay anchored at `tip` stays clear of the map's buttons.
/// `holes` are the map's round buttons and their columns
/// (`dock::MapChrome::holes`, UI points), as the dock placed
/// them: a world-anchored overlay culls itself against them as against the
/// edges in [`map_area`], or a marker paints over a button and takes its tap
/// (2026-11-11, a friend's marker over the right column in Earth mode). The
/// dock writes them in `ClientPhase::Present` before the overlays that read
/// them. A pin or a marker hangs *above* its tip, by up to [`PIN_REACH`], so
/// a tip that far under a column still draws on it.
pub(crate) fn clear_of_chrome(holes: &[Rect], tip: Vec2) -> bool {
    !holes.iter().any(|h| {
        Rect::from_corners(
            h.min - Vec2::new(PIN_SIDE, 0.0),
            h.max + Vec2::new(PIN_SIDE, PIN_REACH),
        )
        .contains(tip)
    })
}

/// How far above its tip a pin or a person's marker reaches, and how far to
/// either side: a head and its stem.
const PIN_REACH: f32 = 56.0;
const PIN_SIDE: f32 = 22.0;

/// Inside the map, and clear of its buttons.
fn shown(holes: &[Rect], map_rect: Rect, tip: Vec2) -> bool {
    map_rect.contains(tip) && clear_of_chrome(holes, tip)
}

/// The part of the viewport a world-anchored overlay may stand on, in UI
/// points: what the side panels left of the screen, minus everything that
/// floats over the map's edges — the keyboard, the dock's bars, the route
/// sheet and the lens.
///
/// Shared with [`crate::systems::locate`]'s device arrow, the place names and
/// the plans, which have the same problem and had no business growing their
/// own copies of this list — the list is the part that goes out of date, one
/// forgotten overlay at a time.
///
/// **The keyboard, the dock and the route sheet are all here for one reason.**
/// The side panels' room says nothing about them, and on a phone the dock is
/// ~240 pt of a 274 pt viewport, so a saved place projecting anywhere near the
/// bottom edge was swallowing taps meant for Search and the app menu, the two
/// controls the whole dock layout exists to put under a thumb.
///
/// The panels' room is last frame's (`UiInput::map_rect`, in window points);
/// before the first frame the whole viewport.
pub(crate) fn map_area(
    input: &UiInput,
    metrics: &UiMetrics,
    keyboard: &crate::systems::onscreen_keyboard::OnScreenKeyboard,
    route: &crate::systems::navigation::RouteState,
    lens: &crate::systems::scenario::LensSlot,
) -> Rect {
    let mut map_rect = input.map_rect().map_or_else(
        || Rect::from_corners(Vec2::ZERO, metrics.viewport_pts()),
        |r| Rect::from_corners(metrics.window_to_ui(r.min), metrics.window_to_ui(r.max)),
    );
    if let Some(slab) = keyboard.slab() {
        if slab.min.y > map_rect.min.y {
            map_rect.max.y = map_rect.max.y.min(slab.min.y);
        } else {
            map_rect.min.y = map_rect.min.y.max(slab.max.y);
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

/// The width of a chat point's label for `name` (`lib_pins::point`): the
/// number's disc, the name (cut at 170 pt) and the label's edges.
#[cfg(feature = "comms")]
fn point_head_width(name: &str) -> f32 {
    4.0 + 22.0 + 7.0 + text_size(name, 12.5, Face::Body, 0.0).x.min(170.0) + 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
    use big_space::prelude::Grid;

    /// A label is measured in the face it is set in: longer words are wider,
    /// letter spacing adds a step per letter, and the line has a height.
    #[test]
    fn labels_are_measured() {
        let short = text_size("Ede", 12.0, Face::Brand, 0.0);
        let long = text_size("Groningen", 12.0, Face::Brand, 0.0);
        assert!(short.x > 0.0 && long.x > short.x);
        assert!(long.y >= 12.0);
        let spaced = text_size("Groningen", 12.0, Face::Brand, 1.4);
        assert!((spaced.x - long.x - 9.0 * 1.4).abs() < 1e-3);
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
