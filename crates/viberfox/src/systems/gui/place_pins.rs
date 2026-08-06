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

use crate::components::FreeCamera;
use crate::resources::MapAnchor;
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
) {
    if store.data.bookmarks.is_empty() {
        return;
    }
    let Some(anchor) = map_anchor.geo else {
        return; // no map yet; nothing to pin to
    };
    let Ok((cam, cam_cell, cam_tf)) = camera.single() else {
        return;
    };

    // The map area: the viewport minus whatever the side panels took. Read off
    // the shared root `Ui` *before* borrowing the context, so both borrows of
    // `egui` don't overlap (the context is a cheap `Arc` clone).
    let map_rect = egui.ui_mut().available_rect_before_wrap();
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

    let mut jump: Option<Teleport> = None;
    let mut drawn = 0usize;
    for b in &store.data.bookmarks {
        let Some(delta) = place_delta(&globe, anchor, cam_world, in_space, b.lat, b.lng, &terrain)
        else {
            continue;
        };
        let Ok(screen) = cam.world_to_viewport(&cam_view, delta.as_vec3()) else {
            continue; // behind the camera
        };
        let tip = egui::pos2(screen.x, screen.y);
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
            jump = Some(Teleport {
                lat: b.lat,
                lng: b.lng,
                altitude: Some(b.altitude),
                pose: None,
            });
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
    if let Some(tp) = jump {
        nav.pending_teleport = Some(tp);
    }
}

/// Where a place is relative to the camera, in metres, or `None` when it should
/// not be drawn at all (over the horizon in space, or out of range on the flat
/// frame).
fn place_delta(
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
        let delta = DVec3::new(wx, f64::from(terrain.at(wx as f32, wz as f32)), wz) - cam_world;
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
    let font = egui::FontId::proportional(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_string(), font, theme::CREAM);
    let pad = egui::vec2(6.0, 3.0);
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
    painter.rect_filled(chip, 4.0, theme::LABEL_CHIP);
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
