//! Bottom-right status overlay: wall-clock time, the camera's geographic
//! position and altitude, and a small compass showing the heading. Laid out as
//! a narrow right-aligned column so it hugs the corner.
//!
//! The clock shows real local time once [`WorldClock`] has learned the
//! region's UTC offset (from the transit feed), and UTC until then.

use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::Region;
use crate::systems::egui_manager::EguiManager;
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE};
use crate::systems::transit_live::WorldClock;
use vibe_core::world::tile_to_meters;

/// Camera lat/lng: world metres → fractional tile → inverse Web Mercator.
fn camera_lat_lng(region: &Region, cell: &GridCell, tf: &Transform) -> (f64, f64) {
    let z = region.tile_z.clamp(0, u32::MAX as i64) as u32;
    let tile_m = tile_to_meters(z, region.latitude);
    let wx = cell.x as f64 * WORLD_CELL_EDGE as f64 + tf.translation.x as f64;
    let wz = cell.z as f64 * WORLD_CELL_EDGE as f64 + tf.translation.z as f64;
    // lat_lng_to_local maps the anchor tile's centre to 0 — invert that.
    let fx = region.tile_x as f64 + 0.5 + wx / tile_m;
    let fy = region.tile_y as f64 + 0.5 + wz / tile_m;
    let n = 2f64.powi(z as i32);
    let lng = fx / n * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * fy / n))
        .sinh()
        .atan()
        .to_degrees();
    (lat, lng)
}

/// Compass bearing of the camera's view direction in degrees (0 = north,
/// 90 = east). North is −Z in world space (East is +X, South is +Z).
fn camera_heading(tf: &Transform) -> Option<f32> {
    let f = tf.rotation * Vec3::NEG_Z;
    let horizontal = f.x * f.x + f.z * f.z;
    (horizontal > 1e-6).then(|| f.x.atan2(-f.z).to_degrees().rem_euclid(360.0))
}

fn cardinal(deg: f32) -> &'static str {
    const DIRS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    DIRS[(((deg + 22.5) / 45.0) as usize) % 8]
}

/// A small compass rose: red half of the needle points to geographic north.
fn compass_widget(ui: &mut egui::Ui, heading_deg: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    p.circle_stroke(c, 8.0, egui::Stroke::new(1.0_f32, egui::Color32::from_gray(110)));
    let th = heading_deg.to_radians();
    // Screen direction of north when "up" is the camera's forward.
    let north = egui::vec2(-th.sin(), -th.cos());
    p.line_segment(
        [c + north * 7.0, c],
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(235, 90, 80)),
    );
    p.line_segment(
        [c, c - north * 5.0],
        egui::Stroke::new(2.0_f32, egui::Color32::from_gray(150)),
    );
}

/// Draw the status overlay in the bottom-right corner of the screen.
pub fn render_hud(
    mut egui: ResMut<EguiManager>,
    clock: Res<WorldClock>,
    regions: Query<&Region>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
) {
    let ctx = egui.ctx_mut();
    let ((h, m, s), is_local) = clock.hms();
    let time = if is_local {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02} UTC")
    };

    let cam = camera.single().ok();
    let geo = cam.and_then(|(cell, tf)| {
        let region = regions.iter().next()?;
        Some(camera_lat_lng(region, cell, tf))
    });
    let altitude = cam.map(|(cell, tf)| cell.y as f32 * WORLD_CELL_EDGE + tf.translation.y);
    let heading = cam.and_then(|(_, tf)| camera_heading(tf));

    let mono = |ui: &mut egui::Ui, text: String, size: f32, gray: u8| {
        ui.label(
            egui::RichText::new(text)
                .monospace()
                .size(size)
                .color(egui::Color32::from_gray(gray)),
        );
    };

    egui::Area::new(egui::Id::new("status_hud"))
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-10.0, -10.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(10, 14, 22, 200))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(9, 6))
                .show(ui, |ui| {
                    // Right-aligned narrow column.
                    ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        mono(ui, time, 15.0, 235);
                        if let Some((lat, lng)) = geo {
                            let ns = if lat >= 0.0 { 'N' } else { 'S' };
                            let ew = if lng >= 0.0 { 'E' } else { 'W' };
                            mono(ui, format!("{:9.5}° {}", lat.abs(), ns), 11.5, 170);
                            mono(ui, format!("{:9.5}° {}", lng.abs(), ew), 11.5, 170);
                        }
                        if let Some(alt) = altitude {
                            mono(ui, format!("alt {:.0} m", alt.max(0.0)), 11.5, 170);
                        }
                        if let Some(deg) = heading {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    mono(
                                        ui,
                                        format!("{} {:03.0}°", cardinal(deg), deg),
                                        11.5,
                                        170,
                                    );
                                    compass_widget(ui, deg);
                                },
                            );
                        }
                    });
                });
        });
}
