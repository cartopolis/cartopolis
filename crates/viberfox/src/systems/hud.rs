//! Bottom-right status overlay: wall-clock time, the camera's geographic
//! position and altitude, a compass, and two location actions — ⧉ copies the
//! current location ("lat, lng, altitude", pasteable into the search bar) and
//! ☆ bookmarks it (named by reverse-geocoded address; see `bookmarks`).
//!
//! The clock shows real local time once [`WorldClock`] has learned the
//! region's UTC offset (from the transit feed), and UTC until then.
//!
//! Layout note: the overlay is an anchored, size-to-content [`egui::Area`].
//! Anything inside that reaches for the *available* width (a `right_to_left`
//! row) would let the box grow a little each frame — the Area sizes to its
//! content, and next frame that larger size becomes the available width, a
//! feedback loop that stretches the panel across the screen and pushes the
//! text off the top. So the content is pinned to [`HUD_WIDTH`] up front, which
//! bounds every inner row and keeps the box a fixed size.

use std::time::Instant;

use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::resources::CameraState;
use crate::systems::bookmarks::{BookmarksState, PLACEHOLDER_NAME};
use crate::systems::egui_manager::EguiManager;
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE};
use crate::systems::geo_nav::{world_to_lat_lng, CameraPose};
use crate::systems::map_stream::MapStream;
use crate::systems::transit_live::WorldClock;
use crate::systems::user_store::UserStore;

/// Fixed content width of the overlay (see the module note on why it must be
/// fixed rather than sized to content).
const HUD_WIDTH: f32 = 150.0;

/// Plain snapshot the overlay renders — no ECS types, so the layout is unit
/// testable.
#[derive(Clone, Default)]
pub struct HudData {
    pub time: String,
    pub geo: Option<(f64, f64)>,
    pub altitude: Option<f64>,
    pub heading: Option<f32>,
    /// Show the copy button's "just copied" ✔ state.
    pub copied: bool,
}

/// What the user clicked in the overlay this frame.
#[derive(Default)]
pub struct HudActions {
    pub copy: bool,
    pub bookmark: bool,
}

/// Diameter (points) of the top-right compass overlay.
const COMPASS_SIZE: f32 = 46.0;

/// Draws the top-right compass rose (Organic-Maps style): the red needle and
/// the "N" point to geographic north as it appears on screen. In globe (Orbit)
/// mode it is a button — clicking eases the view back to north-up; in the
/// free-form fly/avatar modes it is a passive indicator. Returns true when the
/// user clicked it (only possible when `interactive`).
fn compass_overlay(ctx: &egui::Context, heading_deg: f32, interactive: bool) -> bool {
    let mut clicked = false;
    egui::Area::new(egui::Id::new("compass_overlay"))
        // Below the top menu bar, tucked into the right margin.
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 52.0))
        .show(ctx, |ui| {
            let sense = if interactive {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(COMPASS_SIZE, COMPASS_SIZE), sense);
            let hovered = interactive && resp.hovered();
            let p = ui.painter();
            let c = rect.center();
            let r = COMPASS_SIZE * 0.5 - 2.0;
            // Disc + ring (ring brightens on hover to signal it's clickable).
            p.circle_filled(c, r, egui::Color32::from_rgba_unmultiplied(10, 14, 22, 210));
            p.circle_stroke(
                c,
                r,
                egui::Stroke::new(1.5, egui::Color32::from_gray(if hovered { 210 } else { 130 })),
            );
            // Screen direction of north when screen-up is the view heading.
            let th = heading_deg.to_radians();
            let north = egui::vec2(-th.sin(), -th.cos());
            let side = egui::vec2(north.y, -north.x); // 90° clockwise from north
            let w = side * 4.5;
            // North half: red arrow; south half: light grey.
            p.add(egui::Shape::convex_polygon(
                vec![c + north * (r - 3.0), c + w, c - w],
                egui::Color32::from_rgb(230, 80, 70),
                egui::Stroke::NONE,
            ));
            p.add(egui::Shape::convex_polygon(
                vec![c - north * (r - 7.0), c + w, c - w],
                egui::Color32::from_gray(190),
                egui::Stroke::NONE,
            ));
            // "N" over the north tip.
            p.text(
                c + north * (r - 11.0),
                egui::Align2::CENTER_CENTER,
                "N",
                egui::FontId::proportional(10.0),
                egui::Color32::WHITE,
            );
            if interactive {
                resp.clone().on_hover_text("Reset to north-up");
            }
            clicked = resp.clicked();
        });
    clicked
}

/// Build the overlay's anchored box and return what was clicked. Pure UI over
/// [`HudData`], so it can be exercised headless (see the test below).
pub fn hud_area(ctx: &egui::Context, data: &HudData) -> HudActions {
    let mut actions = HudActions::default();
    let mono = |ui: &mut egui::Ui, text: String, size: f32, gray: u8| {
        ui.label(
            egui::RichText::new(text)
                .monospace()
                .size(size)
                .color(egui::Color32::from_gray(gray)),
        );
    };

    // Bottom-left: every panel (AI, Layers, Bookmarks) docks on the right, so
    // the left corner is the one place the overlay never collides with them.
    egui::Area::new(egui::Id::new("status_hud"))
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(10.0, -10.0))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(10, 14, 22, 200))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(9, 6))
                .show(ui, |ui| {
                    // Pin the width first — this is what stops the runaway.
                    ui.set_width(HUD_WIDTH);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        mono(ui, data.time.clone(), 15.0, 235);
                        if let Some((lat, lng)) = data.geo {
                            let ns = if lat >= 0.0 { 'N' } else { 'S' };
                            let ew = if lng >= 0.0 { 'E' } else { 'W' };
                            mono(ui, format!("{:.5}° {}", lat.abs(), ns), 11.5, 170);
                            mono(ui, format!("{:.5}° {}", lng.abs(), ew), 11.5, 170);
                        }
                        if let Some(alt) = data.altitude {
                            let alt = alt.max(0.0);
                            let text = if alt >= 1000.0 {
                                format!("alt {:.1} km", alt / 1000.0)
                            } else {
                                format!("alt {alt:.0} m")
                            };
                            mono(ui, text, 11.5, 170);
                        }
                        // Heading now lives in the top-right compass overlay.
                        // Location actions.
                        if data.geo.is_some() && data.altitude.is_some() {
                            ui.horizontal(|ui| {
                                if ui
                                    .add(egui::Button::new(if data.copied { "✔" } else { "⧉" }))
                                    .on_hover_text(
                                        "Copy location (paste in the search bar to return)",
                                    )
                                    .clicked()
                                {
                                    actions.copy = true;
                                }
                                if ui
                                    .add(egui::Button::new("☆"))
                                    .on_hover_text("Bookmark this location")
                                    .clicked()
                                {
                                    actions.bookmark = true;
                                }
                            });
                        }
                    });
                });
        });
    actions
}

/// Compass bearing (deg, 0 = north, 90 = east) of the direction that reads as
/// "up" on screen over the ground. North is −Z in world space (East +X, South
/// +Z). Uses the camera's forward when it is tilted toward the ground, and its
/// up vector when looking straight down — the two point the same way on the
/// horizontal plane and hand over smoothly, so the compass stays defined
/// through a top-down orbit and out in space (where the old forward-only
/// bearing went undefined at nadir).
fn view_heading(tf: &Transform) -> Option<f32> {
    let fwd = tf.rotation * Vec3::NEG_Z;
    let up = tf.rotation * Vec3::Y;
    let fwd_flat = Vec3::new(fwd.x, 0.0, fwd.z);
    let up_flat = Vec3::new(up.x, 0.0, up.z);
    let h = if fwd_flat.length_squared() >= up_flat.length_squared() {
        fwd_flat
    } else {
        up_flat
    };
    (h.length_squared() > 1e-9).then(|| h.x.atan2(-h.z).to_degrees().rem_euclid(360.0))
}

/// Draw the status overlay and apply its copy / bookmark actions.
#[allow(clippy::too_many_arguments)]
pub fn render_hud(
    mut egui: ResMut<EguiManager>,
    clock: Res<WorldClock>,
    stream: Res<MapStream>,
    mut camera_state: ResMut<CameraState>,
    globe: Res<crate::systems::globe::GlobeState>,
    mut store: ResMut<UserStore>,
    mut bookmarks: ResMut<BookmarksState>,
    data_layers: Res<crate::systems::data_layers::DataLayers>,
    ai: Res<crate::resources::AiAssistantState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    mut copied_at: Local<Option<Instant>>,
) {
    let ((h, m, s), is_local) = clock.hms();
    let cam = camera.single().ok();
    // Geocentric altitude (the local-y "altitude" is wrong away from the anchor).
    let altitude = cam.map(|(cell, tf)| globe.altitude_m(cell, tf));
    let in_space = altitude
        .is_some_and(|a| a as f32 > crate::systems::globe::GLOBE_SHOW_ALT && globe.ready);
    let data = HudData {
        time: if is_local {
            format!("{h:02}:{m:02}:{s:02}")
        } else {
            format!("{h:02}:{m:02}:{s:02} UTC")
        },
        geo: cam.zip(stream.geo_anchor()).map(|((cell, tf), anchor)| {
            if in_space {
                // Over the globe: the flat-frame mapping is meaningless far from
                // the anchor — show the geographic sub-point instead.
                let edge = WORLD_CELL_EDGE as f64;
                let cam_w = bevy::math::DVec3::new(
                    cell.x as f64 * edge + tf.translation.x as f64,
                    cell.y as f64 * edge + tf.translation.y as f64,
                    cell.z as f64 * edge + tf.translation.z as f64,
                );
                let dir = (cam_w - globe.center_local.as_dvec3())
                    .normalize_or_zero()
                    .as_vec3();
                crate::systems::globe::sub_point(globe.anchor_lat, globe.anchor_lng, dir)
            } else {
                let wx = cell.x as f64 * WORLD_CELL_EDGE as f64 + tf.translation.x as f64;
                let wz = cell.z as f64 * WORLD_CELL_EDGE as f64 + tf.translation.z as f64;
                world_to_lat_lng(anchor, wx, wz)
            }
        }),
        altitude,
        heading: cam.and_then(|(_, tf)| view_heading(tf)),
        copied: copied_at.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5),
    };

    let actions = hud_area(egui.ctx_mut(), &data);

    // Top-right compass. Clicking it resets to north-up in globe (Orbit) mode
    // only; fly/avatar are free-form, so there it is a passive indicator.
    //
    // A right-docked panel (Layers, Bookmarks, AI) sits opaque over this same
    // corner. egui always paints floating Areas on top of panels (panels live
    // on the bottom-most background layer, and even `Order::Background` Areas
    // paint above it), so the only way to keep the compass *behind* the panel
    // is to drop it while one is open.
    let right_panel_open = data_layers.panel_open || bookmarks.panel_open || ai.open;
    if let Some(heading) = data.heading.filter(|_| !right_panel_open) {
        let orbit = matches!(camera_state.mode, crate::resources::CameraMode::Orbit);
        if compass_overlay(egui.ctx_mut(), heading, orbit) {
            camera_state.orbit_north_reset = true;
        }
    }

    if actions.copy {
        if let (Some((lat, lng)), Some(alt)) = (data.geo, data.altitude) {
            crate::utils::clipboard::set_text(&format!("{lat:.5}, {lng:.5}, {alt:.0}"));
            *copied_at = Some(Instant::now());
        }
    }
    if actions.bookmark {
        if let (Some((lat, lng)), Some(alt)) = (data.geo, data.altitude) {
            // Capture the current view so the bookmark returns to it exactly.
            let pose = CameraPose::capture(&camera_state);
            let id = store.add_bookmark(PLACEHOLDER_NAME.to_string(), lat, lng, alt, pose);
            bookmarks.request_address(id, lat, lng);
            bookmarks.panel_open = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlay must not grow across frames: a size-to-content anchored
    /// Area with an unbounded inner row would stretch a little each frame
    /// (the bug this guards against). Render several frames alongside the side
    /// panels that shrink the central area, and assert the box stays put.
    #[test]
    fn hud_box_does_not_grow() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = HudData {
            time: "08:14:11".to_string(),
            geo: Some((53.21952, 6.58220)),
            altitude: Some(17.0),
            heading: Some(247.0),
            copied: false,
        };
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1323.0, 819.0));

        let mut sizes = Vec::new();
        for _ in 0..10 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            let out = ctx.run(raw, |ctx| {
                // Side panels present, like the live app.
                egui::SidePanel::right("layers_panel")
                    .default_width(260.0)
                    .show(ctx, |ui| {
                        ui.heading("Layers");
                    });
                egui::SidePanel::right("bookmarks_panel")
                    .default_width(280.0)
                    .show(ctx, |ui| {
                        ui.heading("Bookmarks");
                    });
                hud_area(ctx, &data);
            });
            // The HUD's frame is the largest translucent rect (the side panels
            // are opaque). `from_rgba_unmultiplied` premultiplies internally,
            // and the frame fades in over the first frames, so match the alpha
            // band loosely and take the biggest.
            let rect = out
                .shapes
                .iter()
                .filter_map(|cs| match &cs.shape {
                    egui::Shape::Rect(rs) if (1..250).contains(&rs.fill.a()) => {
                        Some(cs.shape.visual_bounding_rect())
                    }
                    _ => None,
                })
                .max_by(|a, b| (a.area()).total_cmp(&b.area()));
            sizes.push(rect.unwrap_or(egui::Rect::NOTHING));
        }

        // On the settled frames the box must be compact, on-screen, and stable
        // — the regression being an anchored Area that grows every frame.
        let last = *sizes.last().unwrap();
        let prev = sizes[sizes.len() - 2];
        assert!(last.width() < 220.0, "HUD too wide: {}", last.width());
        assert!(last.height() < 180.0, "HUD too tall: {}", last.height());
        assert!(last.min.y >= 0.0, "HUD grew off the top: {}", last.min.y);
        assert!(screen.contains_rect(last), "HUD outside the screen: {last:?}");
        assert!(
            (last.width() - prev.width()).abs() < 0.5
                && (last.height() - prev.height()).abs() < 0.5,
            "HUD size still changing: {prev:?} -> {last:?}"
        );
    }
}
