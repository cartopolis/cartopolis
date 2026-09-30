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
//!
//! Below the space tier a place is placed through the anchored Mercator frame
//! (`geo_nav::lat_lng_to_world`) — the same frame the tiles are laid in, so the
//! pin sits on its building. In space that frame is meaningless, so the point is
//! taken on the globe itself (`globe::GlobeState::direction_local`) and pins on
//! the far side are dropped by the horizon test.

use crate::components::{Avatar, FreeCamera, RemoteAvatar};
use crate::resources::MapAnchor;
use crate::systems::chat_people;
use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::grid::WORLD_CELL_EDGE;
use crate::systems::egui_manager::EguiManager;
use crate::systems::geo_nav::{lat_lng_to_world, GeoNavState, Teleport};
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
/// First pin id used for a chat's points: far above any bookmark id, which
/// counts up from 1.
const CHAT_PIN_IDS: u64 = 1 << 48;
/// First pin id used for people's shared locations.
const PEOPLE_PIN_IDS: u64 = 1 << 49;

/// The "you are sharing your real location" chip: top centre of the screen,
/// naming who can see it and for how long, with a Stop for each. Answers the
/// chat whose Stop was pressed.
///
/// Drawn from this system because it is the last UI system to run (the module
/// note says why), so nothing composed later can cover it.
fn sharing_chip(ctx: &egui::Context, chat: &crate::systems::chat::ChatState) -> Option<String> {
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut stop = None;
    egui::Area::new(egui::Id::new("real_location_sharing_chip"))
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 8.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::REAL_LOCATION)
                .corner_radius(12.0)
                .inner_margin(egui::Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    for share in &chat.sharing {
                        let who = chat
                            .channels
                            .iter()
                            .find(|c| c.key == share.channel)
                            .map(|c| {
                                crate::systems::private_chat::peer_of_label(&c.label).to_string()
                            })
                            .unwrap_or_else(|| "a chat".into());
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} {who} sees where your phone is · {} min",
                                    crate::systems::icons::HERE,
                                    share.until_ms.saturating_sub(now) / 60_000 + 1
                                ))
                                .color(theme::INK),
                            );
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new("Stop").color(theme::INK).strong(),
                                ))
                                .clicked()
                            {
                                stop = Some(share.channel.clone());
                            }
                        });
                    }
                });
        });
    stop
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
    mut egui: ResMut<EguiManager>,
    store: Res<UserStore>,
    mut nav: ResMut<GeoNavState>,
    globe: Res<GlobeState>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    map_anchor: Res<MapAnchor>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    driver: Res<crate::systems::driver::DriverState>,
    route: Res<crate::systems::navigation::RouteState>,
    // The points of the private chat whose World view is on (the chat panel's
    // "Map" toggle). Drawn by the same code as saved places so they share the
    // projection, the culling and the one ordering rule this system obeys.
    //
    // Also where other people's shared real locations are pinned, and where the
    // "you are sharing your location" chip is drawn — see [`sharing_chip`].
    mut chat: ResMut<crate::systems::chat::ChatState>,
    // The people in that chat, where their avatars are (`chat_people`): the
    // server's answer, and this client's own avatars to stand a marker on
    // when one of them is in view.
    contacts: Res<crate::systems::contacts::Contacts>,
    remotes: Query<(&RemoteAvatar, &Transform), Without<FreeCamera>>,
    local: Query<(&GridCell, &Transform), (With<Avatar>, Without<FreeCamera>)>,
) {
    // Before everything else, driver mode included: while anybody can see where
    // this phone is, the screen says so. A share that is on without a sign of it
    // is the one thing this feature must never be.
    if !chat.sharing.is_empty() {
        let ctx = egui.ctx_mut().clone();
        if let Some(channel) = sharing_chip(&ctx, &chat) {
            chat.stop_sharing(&channel);
        }
    }
    // Driver mode owns the screen and strips the map to the route — but this
    // system never learned that, so the teardrops kept drawing over the
    // navigation view, and every one of them is a live `Area` whose click is a
    // `Teleport`. Tapping a saved place at 50 km/h is not a gesture that should
    // be reachable.
    if driver.active {
        return;
    }
    let chat_points = chat.world_points();
    let now_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let people: Vec<crate::systems::chat::SharedLocation> = chat
        .locations
        .iter()
        .filter(|l| l.until_ms > now_ms)
        .cloned()
        .collect();
    let chat_world = chat
        .world
        .clone()
        .filter(|k| crate::systems::chat::ChatState::is_private(k));
    if store.data.bookmarks.is_empty()
        && chat_points.is_empty()
        && people.is_empty()
        && chat_world.is_none()
    {
        return;
    }
    let Some(anchor) = map_anchor.geo else {
        return; // no map yet; nothing to pin to
    };
    let Ok((cam, cam_cell, cam_tf)) = camera.single() else {
        return;
    };

    let map_rect = map_area(&mut egui, &keyboard, &route);
    // Projection answers in window points; the pins are drawn in egui's. Bound
    // once here rather than called per pin, so the `ResMut` isn't re-borrowed
    // inside the loop that owns `ctx`.
    let ui_scale = egui.ui_scale();
    let to_ui = |p: Vec2| egui::pos2(p.x / ui_scale, p.y / ui_scale);
    let ctx = egui.ctx_mut().clone();
    if map_rect.width() < 2.0 * HEAD_R || map_rect.height() < 3.0 * HEAD_R {
        return; // panels have taken the screen (compact layout)
    }

    let edge = WORLD_CELL_EDGE as f64;
    let cam_world = DVec3::new(
        cam_cell.x as f64 * edge + cam_tf.translation.x as f64,
        cam_cell.y as f64 * edge + cam_tf.translation.y as f64,
        cam_cell.z as f64 * edge + cam_tf.translation.z as f64,
    );
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

    // Carries the place's name alongside the teleport: the address bar's recents
    // list reads `pending_label`, and a jump that arrives without one shows up
    // there as a bare coordinate pair rather than as the place you saved.
    let mut jump: Option<(Teleport, String)> = None;
    let mut drawn = 0usize;
    for b in &store.data.bookmarks {
        let Some(delta) = place_delta(&globe, anchor, cam_world, in_space, b.lat, b.lng, &terrain)
        else {
            continue;
        };
        let Ok(screen) = cam.world_to_viewport(&cam_view, delta.as_vec3()) else {
            continue; // behind the camera
        };
        let tip = to_ui(screen);
        if !map_rect.contains(tip) {
            continue;
        }
        drawn += 1;
        if pin(
            &ctx,
            map_rect,
            b.id,
            tip,
            store.pin_color(b.folder),
            &b.name,
        ) {
            // Same jump the panel's row does: keep the current camera mode and
            // move it to the spot (`pose: None`).
            jump = Some((
                Teleport {
                    lat: b.lat,
                    lng: b.lng,
                    altitude: Some(b.altitude),
                    pose: None,
                    silent: false,
                },
                b.name.clone(),
            ));
        }
    }
    // A chat's points, as the design's numbered labels ([`point_pin`]) rather
    // than teardrops, so they read as "from the conversation" and never as one
    // of your own saved places. Ids from a range no bookmark reaches, since egui
    // keys each pin's `Area` by it.
    for (i, (point, label)) in chat_points.iter().enumerate() {
        let Some(delta) = place_delta(
            &globe,
            anchor,
            cam_world,
            in_space,
            point.latitude,
            point.longitude,
            &terrain,
        ) else {
            continue;
        };
        let Ok(screen) = cam.world_to_viewport(&cam_view, delta.as_vec3()) else {
            continue;
        };
        let tip = to_ui(screen);
        if !map_rect.contains(tip) {
            continue;
        }
        drawn += 1;
        // `world_points` numbers each label "<n>  <name>", matching the card.
        let (number, name) = label.split_once("  ").unwrap_or(("", label.as_str()));
        if point_pin(&ctx, map_rect, CHAT_PIN_IDS + i as u64, tip, number, name) {
            jump = Some((Teleport::point(point), label.clone()));
        }
    }
    // People's real locations, shared with a private chat, in the blue that
    // means nothing else (`theme::REAL_LOCATION`).
    let blue = theme::REAL_LOCATION;
    let blue = [
        f32::from(blue.r()) / 255.0,
        f32::from(blue.g()) / 255.0,
        f32::from(blue.b()) / 255.0,
    ];
    for (i, l) in people.iter().enumerate() {
        let Some(delta) = place_delta(
            &globe,
            anchor,
            cam_world,
            in_space,
            l.latitude,
            l.longitude,
            &terrain,
        ) else {
            continue;
        };
        let Ok(screen) = cam.world_to_viewport(&cam_view, delta.as_vec3()) else {
            continue;
        };
        let tip = to_ui(screen);
        if !map_rect.contains(tip) {
            continue;
        }
        drawn += 1;
        let label = format!("{} (where their phone is)", l.from);
        if pin(&ctx, map_rect, PEOPLE_PIN_IDS + i as u64, tip, blue, &label) {
            jump = Some((Teleport::geo(l.latitude, l.longitude, None), l.from.clone()));
        }
    }
    // The people in the chat whose World view is open, at their avatars, with
    // what each last said floating over them (`chat_people`, design v5 B).
    if let Some(key) = chat_world {
        let now = chat.clock;
        // Over the head on the flat map; from orbit a head is not a distance.
        let head_up = if in_space {
            DVec3::ZERO
        } else {
            DVec3::Y * chat_people::HEAD_HEIGHT_M
        };
        let mut balloons: Vec<(String, egui::Pos2, String, f32, bool)> = Vec::new();
        for w in &contacts.whereabouts {
            let (lat, lng) = w.at(now);
            let Some(ground) = place_delta(&globe, anchor, cam_world, in_space, lat, lng, &terrain)
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
            let Ok(screen) = cam.world_to_viewport(&cam_view, (foot + head_up).as_vec3()) else {
                continue;
            };
            let tip = to_ui(screen);
            if !map_rect.contains(tip) {
                continue;
            }
            drawn += 1;
            if chat_people::marker(&ctx, map_rect, tip, &w.username) {
                jump = Some((Teleport::geo(lat, lng, None), w.username.clone()));
            }
            if let Some((text, alpha)) = chat_people::latest_said(&chat, &key, &w.username) {
                let over = egui::pos2(tip.x, tip.y - chat_people::marker_height());
                balloons.push((w.username.clone(), over, text, alpha, false));
            }
        }
        // Your own last line, over your own avatar, which has no marker: it is
        // the one person on the map you do not need to find.
        if let (false, Some((text, alpha)), Ok((cell, tf))) = (
            in_space,
            chat_people::latest_mine(&chat, &key),
            local.single(),
        ) {
            let foot = DVec3::new(
                cell.x as f64 * edge + tf.translation.x as f64,
                cell.y as f64 * edge + tf.translation.y as f64,
                cell.z as f64 * edge + tf.translation.z as f64,
            ) - cam_world;
            if let Ok(screen) = cam.world_to_viewport(&cam_view, (foot + head_up).as_vec3()) {
                let over = to_ui(screen);
                if map_rect.contains(over) {
                    balloons.push((String::new(), over, text, alpha, true));
                }
            }
        }
        // After every marker, so a balloon is never under somebody's disc.
        for (who, over, text, alpha, mine) in balloons {
            chat_people::balloon(&ctx, map_rect, &who, over, &text, alpha, mine);
        }
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
pub(crate) fn map_area(
    egui: &mut EguiManager,
    keyboard: &crate::systems::onscreen_keyboard::OnScreenKeyboard,
    route: &crate::systems::navigation::RouteState,
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
        map_rect.max.y = map_rect.max.y.min(sheet.top());
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
        let delta =
            DVec3::new(wx, f64::from(terrain.ground_at(wx as f32, wz as f32)), wz) - cam_world;
        (delta.x.hypot(delta.z) < MAX_FLAT_RANGE_M).then_some(delta)
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
    id: u64,
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
                    theme::CREAM.r(),
                    theme::CREAM.g(),
                    theme::CREAM.b(),
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

/// A chat's point on its World view: the design's `.ppin` — a cream label with
/// the point's number in an ink circle and its name, on a thin stem down to a
/// dot on the ground. Always labelled: a conversation has a handful of places,
/// and the number is what ties each to its card in the thread.
fn point_pin(
    ctx: &egui::Context,
    map_rect: egui::Rect,
    id: u64,
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
            let p = ui.painter();
            let mut job = egui::text::LayoutJob::simple_singleline(
                name.to_string(),
                font.clone(),
                theme::INK,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(170.0);
            let galley = p.layout_job(job);
            let head_w = 4.0 + 22.0 + 7.0 + galley.size().x + 10.0;
            let (_rect, response) = ui.allocate_exact_size(
                egui::vec2(head_w, HEAD_H + STEM + DOT_R * 2.0),
                egui::Sense::click(),
            );
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
                theme::CREAM,
            );
            p.circle(dot, DOT_R, theme::CREAM, egui::Stroke::new(2.0, theme::INK));
            let fill = if response.hovered() {
                egui::Color32::WHITE
            } else {
                theme::CREAM
            };
            p.rect_filled(head, egui::CornerRadius::same(14), fill);
            let n = egui::pos2(head.left() + 4.0 + 11.0, head.center().y);
            p.circle_filled(n, 11.0, theme::INK);
            p.text(
                n,
                egui::Align2::CENTER_CENTER,
                number,
                egui::FontId::proportional(12.0),
                theme::CREAM,
            );
            let gs = galley.size();
            p.galley(
                egui::pos2(n.x + 11.0 + 7.0, head.center().y - gs.y / 2.0),
                galley,
                theme::INK,
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
    painter.circle_filled(centre, r * HOLE_R, theme::CREAM);
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
        .layout_no_wrap(name.to_string(), font, theme::CREAM);
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
    painter.galley(chip.min + pad, galley, theme::CREAM);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::WORLD_SWITCH_THRESHOLD;
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
