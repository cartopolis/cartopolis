//! The rungs above the globe: Earth & Moon, the Solar system, the Stars and the
//! Milky Way (`docs/mockups/zoom-ladder/`). Past the globe's ceiling the 3D
//! camera waits over the place it left and the zoom moves
//! `CameraState::cosmos_alt` instead; this paints what is out there, behind
//! every panel, fading in over the globe as the height passes the ceiling.
//!
//! **Real bodies at real angles, drawn to the board's scale.** Earth & Moon is to
//! scale, seen from the waiting camera: the Earth lit by the real Sun, the Moon
//! where the sky code puts it (`utils::sun`), its orbit through both. The Solar
//! system is the ecliptic from above, the planets where JPL's elements put them
//! today (`utils::planets`), distances log-scaled — a linear scale would leave
//! every inner planet inside one pixel of the Sun — and says so. The Stars are
//! the systems within 16 light years (`utils::near_stars`, HYG) on the
//! celestial equator's plane, to scale. The Milky Way is schematic: four arms
//! on a log spiral, the Sun 26,000 light years out, and it says that too.
//!
//! **Home stays in view.** Each board is centred on what you came from — the
//! Earth, then the Sun — and eases to its own middle only as you pull back, so
//! a climb never loses the thread. Zooming back in heads for the Earth; there is
//! nowhere else to land.
//!
//! Painted with egui on the background layer, before the panel chain, so the
//! panels and the map's chrome stay on top.

use crate::systems::egui_manager::EguiRef;
use crate::systems::theme;
use crate::utils::near_stars::NEAR_STARS;
use crate::utils::planets::{self, AU_M, LIGHT_M_S, LIGHT_YEAR_M, PLANETS};
use bevy::prelude::*;
use egui::{pos2, vec2, Color32, Pos2};

/// The Earth's radius, metres, for the to-scale board.
const EARTH_R_M: f64 = 6_371_000.0;
/// The Moon's.
const MOON_R_M: f64 = 1_737_400.0;
/// The Moon's mean distance, metres.
const MOON_DIST_M: f64 = 384_400_000.0;
/// The Sun's distance from the galactic centre, light years.
const SUN_GALACTIC_LY: f64 = 26_000.0;

/// Deep space behind the boards.
const SPACE: Color32 = Color32::from_rgb(0x04, 0x07, 0x0A);
const EARTH_DAY: Color32 = Color32::from_rgb(0x5D, 0x8F, 0xBF);
const EARTH_NIGHT: Color32 = Color32::from_rgb(0x0C, 0x1A, 0x2A);
const MOON_DAY: Color32 = Color32::from_rgb(0xD9, 0xD6, 0xCC);
const MOON_NIGHT: Color32 = Color32::from_rgb(0x26, 0x29, 0x2C);
const SUN: Color32 = Color32::from_rgb(0xFF, 0xE8, 0xA8);
const ORBIT: Color32 = Color32::from_rgba_unmultiplied_const(0xA0, 0xBE, 0xC8, 70);
const LANE: Color32 = Color32::from_rgba_unmultiplied_const(0x5C, 0xC6, 0xC0, 60);
/// Planet colours, Mercury to Neptune: a palette for content, not chrome.
const PLANET_TONES: [Color32; 8] = [
    Color32::from_rgb(0xB5, 0xAE, 0xA4),
    Color32::from_rgb(0xF1, 0xD9, 0x9A),
    EARTH_DAY,
    Color32::from_rgb(0xE0, 0x7A, 0x4F),
    Color32::from_rgb(0xE8, 0xC4, 0x9A),
    Color32::from_rgb(0xF0, 0xDC, 0xA0),
    Color32::from_rgb(0x9F, 0xE0, 0xE6),
    Color32::from_rgb(0x5B, 0x84, 0xE6),
];

/// 0 below `lo`, 1 above `hi`, smooth on a log scale between.
fn fade_in(alt: f64, lo: f64, hi: f64) -> f32 {
    let t = ((alt.ln() - lo.ln()) / (hi.ln() - lo.ln())).clamp(0.0, 1.0) as f32;
    t * t * (3.0 - 2.0 * t)
}

fn fade_out(alt: f64, lo: f64, hi: f64) -> f32 {
    1.0 - fade_in(alt, lo, hi)
}

fn alpha(c: Color32, a: f32) -> Color32 {
    c.gamma_multiply(a.clamp(0.0, 1.0))
}

/// A distance in words: kilometres, then astronomical units, then light years.
fn distance_words(m: f64) -> String {
    if m < 0.05 * AU_M {
        format!("{} km", thousands((m / 1000.0).round() as u64))
    } else if m < 0.05 * LIGHT_YEAR_M {
        format!("{:.2} AU", m / AU_M)
    } else {
        format!("{:.1} light years", m / LIGHT_YEAR_M)
    }
}

/// How long light takes over a distance, in words.
fn light_time_words(m: f64) -> String {
    let s = m / LIGHT_M_S;
    if s < 60.0 {
        format!("{s:.1} light-seconds")
    } else if s < 3_600.0 {
        format!("{} min of light", (s / 60.0).round())
    } else {
        format!("{:.1} h of light", s / 3_600.0)
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A label with a dark halo, so it reads over anything.
fn label(p: &egui::Painter, at: Pos2, align: egui::Align2, text: &str, size: f32, ink: Color32) {
    let font = egui::FontId::proportional(size);
    let halo = alpha(theme::INK, ink.a() as f32 / 255.0);
    for d in [
        vec2(1.0, 0.0),
        vec2(-1.0, 0.0),
        vec2(0.0, 1.0),
        vec2(0.0, -1.0),
    ] {
        p.text(at + d, align, text, font.clone(), halo);
    }
    p.text(at, align, text, font, ink);
}

/// A sphere seen from the camera, lit from one side: the dark disc, then the
/// lit part as a strip between the limb and the terminator's ellipse (a strip
/// fills a crescent as well as a gibbous phase, which a convex polygon cannot).
/// `sun` is the sun's direction on screen (x right, y up); `toward` how far it
/// stands towards the viewer, −1 behind the sphere to 1 behind the viewer.
fn paint_lit_sphere(
    p: &egui::Painter,
    c: Pos2,
    r: f32,
    sun: Vec2,
    toward: f32,
    day: Color32,
    night: Color32,
) {
    p.circle_filled(c, r, night);
    if r < 1.5 {
        p.circle_filled(c, r.max(1.0), day);
        return;
    }
    let u = if sun.length_squared() > 1e-8 {
        sun.normalize()
    } else {
        Vec2::X
    };
    // Screen y runs down.
    let (ux, uy) = (u.x, -u.y);
    let (vx, vy) = (-uy, ux);
    let mut mesh = egui::Mesh::default();
    let n = 32;
    for i in 0..=n {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / n as f32;
        let (s, co) = a.sin_cos();
        let limb = pos2(c.x + r * (co * ux + s * vx), c.y + r * (co * uy + s * vy));
        let term = pos2(
            c.x + r * (-toward * co * ux + s * vx),
            c.y + r * (-toward * co * uy + s * vy),
        );
        mesh.colored_vertex(limb, day);
        mesh.colored_vertex(term, day);
        if i > 0 {
            let k = 2 * i as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    p.add(egui::Shape::mesh(mesh));
}

/// Paint the rungs above the globe, while the view is out there.
#[allow(clippy::too_many_arguments)]
pub fn render_cosmos(
    egui: EguiRef,
    camera_state: Res<crate::resources::CameraState>,
    dev: Res<crate::resources::DevPanelState>,
    clock: Res<crate::systems::transit_live::WorldClock>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<(&Transform, &Projection), With<crate::components::FreeCamera>>,
    mut galaxy: Local<Vec<(Vec2, f32)>>,
) {
    let alt = camera_state.cosmos_alt;
    if alt <= 0.0 {
        return;
    }
    let Ok((tf, projection)) = camera.single() else {
        return;
    };
    let ctx = egui.ctx();
    let screen = ctx.content_rect();
    let p = ctx.layer_painter(egui::LayerId::background());
    let ceiling = alt.min(45_000_000.0 - EARTH_R_M);
    let backdrop = fade_in(alt, ceiling.max(3.0e7), 1.0e8);
    p.rect_filled(screen, 0.0, alpha(SPACE, backdrop));

    let fov = match projection {
        Projection::Perspective(pp) => pp.fov,
        _ => std::f32::consts::FRAC_PI_4,
    };
    let half_h = screen.height() * 0.5;
    let focal = half_h / (fov * 0.5).tan();
    let centre = screen.center();
    let now = clock.now_unix;

    // The camera's frame, render space, which the sun and moon are in too.
    let right = (tf.rotation * Vec3::X).as_dvec3();
    let up = (tf.rotation * Vec3::Y).as_dvec3();
    let out = camera_state.orbit_dir.normalize();
    // The real sun and moon, whatever the light setting holds the street at.
    let hour = crate::utils::sun::solar_time_of_day(now, globe.anchor_lng);
    let sun = crate::utils::sun::sun_direction(hour, dev.sun_declination_rad).as_dvec3();
    let age = crate::utils::sun::lunar_age_days(now);
    let moon_dir = crate::utils::sun::moon_direction(hour, age).as_dvec3();
    let on_screen = |d: bevy::math::DVec3| Vec2::new(d.dot(right) as f32, d.dot(up) as f32);
    let ink = theme::TEXT;

    // ── Earth & Moon, to scale ──────────────────────────────────────────────
    let em = fade_out(alt, 1.5e10, 6.0e10) * backdrop;
    if em > 0.01 {
        let eye = EARTH_R_M + alt;
        let project = |pt: bevy::math::DVec3| -> Option<(Pos2, f64)> {
            let z = eye - pt.dot(out);
            (z > 1.0).then(|| {
                let x = pt.dot(right) / z * f64::from(focal);
                let y = pt.dot(up) / z * f64::from(focal);
                (pos2(centre.x + x as f32, centre.y - y as f32), z)
            })
        };
        // The Moon's orbit, through where it is now and where it will be in a
        // few hours: the plane the sky code moves it in.
        let later = crate::utils::sun::moon_direction(hour + 3.0, age + 0.125).as_dvec3();
        let normal = moon_dir.cross(later).normalize_or_zero();
        if normal != bevy::math::DVec3::ZERO {
            let a = moon_dir.normalize();
            let b = normal.cross(a);
            let ring: Vec<Pos2> = (0..=96)
                .filter_map(|i| {
                    let t = std::f64::consts::TAU * f64::from(i) / 96.0;
                    project((a * t.cos() + b * t.sin()) * MOON_DIST_M).map(|(q, _)| q)
                })
                .collect();
            p.add(egui::Shape::line(
                ring,
                egui::Stroke::new(1.0, alpha(ORBIT, em)),
            ));
        }
        let earth_r = (focal as f64 * (EARTH_R_M / eye.max(EARTH_R_M * 1.01)).asin().tan()) as f32;
        let toward = sun.dot(out) as f32;
        paint_lit_sphere(
            &p,
            centre,
            earth_r.max(2.0),
            on_screen(sun),
            toward,
            alpha(EARTH_DAY, em),
            alpha(EARTH_NIGHT, em),
        );
        p.circle_stroke(
            centre,
            earth_r.max(2.0) + 6.0,
            egui::Stroke::new(2.0, alpha(theme::BRAND, em)),
        );
        label(
            &p,
            centre + vec2(0.0, earth_r.max(2.0) + 12.0),
            egui::Align2::CENTER_TOP,
            "Earth",
            theme::SIZE_BODY,
            alpha(theme::CREAM, em),
        );
        if let Some((m, z)) = project(moon_dir * MOON_DIST_M) {
            let mr = (f64::from(focal) * MOON_R_M / z) as f32;
            paint_lit_sphere(
                &p,
                m,
                mr.max(2.5),
                on_screen(sun),
                toward,
                alpha(MOON_DAY, em),
                alpha(MOON_NIGHT, em),
            );
            label(
                &p,
                m + vec2(8.0, -4.0),
                egui::Align2::LEFT_BOTTOM,
                "Moon",
                theme::SIZE_BODY,
                alpha(theme::CREAM, em),
            );
            label(
                &p,
                m + vec2(8.0, 0.0),
                egui::Align2::LEFT_TOP,
                &format!(
                    "{} · {}",
                    distance_words(MOON_DIST_M),
                    light_time_words(MOON_DIST_M)
                ),
                theme::SIZE_SUB,
                alpha(ink, em),
            );
        }
        // Where the Sun is: an arrow at the edge of the screen.
        let s = on_screen(sun);
        if s.length() > 1e-3 {
            let d = s.normalize();
            let edge = centre + vec2(d.x, -d.y) * (half_h * 0.86);
            let au = planets::position(&PLANETS[planets::EARTH], now);
            let sun_m = (au[0] * au[0] + au[1] * au[1] + au[2] * au[2]).sqrt() * AU_M;
            p.circle_filled(edge, 5.0, alpha(SUN, em));
            label(
                &p,
                edge + vec2(0.0, 10.0),
                egui::Align2::CENTER_TOP,
                &format!("to the Sun · {}", light_time_words(sun_m)),
                theme::SIZE_SUB,
                alpha(SUN, em),
            );
        }
    }

    // ── The Solar system, the ecliptic from above, log distances ────────────
    let ss = fade_in(alt, 1.0e10, 4.0e10) * fade_out(alt, 1.0e14, 5.0e14) * backdrop;
    if ss > 0.01 {
        let rmax = (alt / AU_M * 2.5).clamp(1.6, 45.0);
        let rpx = half_h * 0.9;
        let radial =
            |r: f64| -> f32 { rpx * ((1.0 + r / 0.25).ln() / (1.0 + rmax / 0.25).ln()) as f32 };
        let place = |xyz: [f64; 3]| -> Vec2 {
            let r = (xyz[0] * xyz[0] + xyz[1] * xyz[1]).sqrt();
            let a = xyz[1].atan2(xyz[0]);
            Vec2::new(a.cos() as f32, -(a.sin() as f32)) * radial(r)
        };
        let earth_xyz = planets::position(&PLANETS[planets::EARTH], now);
        let earth_at = place(earth_xyz);
        // Earth in the middle as the board arrives, the Sun once it has.
        let settle = fade_in(alt, 2.0e10, 3.0e11);
        let sun_at = centre - egui::Vec2::new(earth_at.x, earth_at.y) * (1.0 - settle);
        let glow = alpha(SUN, ss * 0.25);
        p.circle_filled(sun_at, 16.0, glow);
        p.circle_filled(sun_at, 9.0, alpha(SUN, ss));
        label(
            &p,
            sun_at + vec2(0.0, 14.0),
            egui::Align2::CENTER_TOP,
            "Sun",
            theme::SIZE_BODY,
            alpha(SUN, ss),
        );
        for (i, planet) in PLANETS.iter().enumerate() {
            let xyz = planets::position(planet, now);
            let r = (xyz[0] * xyz[0] + xyz[1] * xyz[1]).sqrt();
            let orbit_r = radial(r);
            if orbit_r > half_h * 4.0 {
                continue;
            }
            p.circle_stroke(sun_at, orbit_r, egui::Stroke::new(1.0, alpha(ORBIT, ss)));
            let at = place(xyz);
            let q = sun_at + egui::Vec2::new(at.x, at.y);
            let size = if i >= 4 { 6.0 } else { 4.5 };
            p.circle_filled(q, size, alpha(PLANET_TONES[i], ss));
            let words = if i == planets::EARTH {
                p.circle_stroke(
                    q,
                    size + 5.0,
                    egui::Stroke::new(2.0, alpha(theme::BRAND, ss)),
                );
                "Earth · you".to_string()
            } else {
                let d = (0..3)
                    .map(|k| (xyz[k] - earth_xyz[k]).powi(2))
                    .sum::<f64>()
                    .sqrt()
                    * AU_M;
                format!("{} · {}", planet.name, light_time_words(d))
            };
            label(
                &p,
                q + vec2(0.0, size + 4.0),
                egui::Align2::CENTER_TOP,
                &words,
                theme::SIZE_SUB,
                alpha(theme::CREAM, ss),
            );
        }
        label(
            &p,
            pos2(screen.left() + 16.0, screen.bottom() - 16.0),
            egui::Align2::LEFT_BOTTOM,
            "The planets where they are today · distances log-scaled",
            theme::SIZE_SUB,
            alpha(theme::TEXT_DIM, ss),
        );
    }

    // ── The Stars within 16 light years, to scale ───────────────────────────
    let st = fade_in(alt, 1.0e14, 1.0e15) * fade_out(alt, 1.0e18, 5.0e18) * backdrop;
    if st > 0.01 {
        let px_per_ly = (f64::from(focal) / (alt / LIGHT_YEAR_M)) as f32;
        let ly_per_pc = 3.261_563_8_f32;
        let at = |s: &crate::utils::near_stars::NearStar| {
            centre + vec2(s.xyz_pc[0], -s.xyz_pc[1]) * ly_per_pc * px_per_ly
        };
        // Each system's lane to its nearest neighbour (scenarios v3's lanes).
        for (i, a) in NEAR_STARS.iter().enumerate() {
            let near = NEAR_STARS
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .min_by(|(_, x), (_, y)| {
                    let dx = |s: &crate::utils::near_stars::NearStar| {
                        (0..3)
                            .map(|k| (s.xyz_pc[k] - a.xyz_pc[k]).powi(2))
                            .sum::<f32>()
                    };
                    dx(x).total_cmp(&dx(y))
                });
            if let Some((_, b)) = near {
                p.line_segment([at(a), at(b)], egui::Stroke::new(1.0, alpha(LANE, st)));
            }
        }
        // The Sun's lanes to the three nearest.
        for s in NEAR_STARS.iter().take(3) {
            p.line_segment([centre, at(s)], egui::Stroke::new(1.0, alpha(LANE, st)));
        }
        p.circle_filled(centre, 5.0, alpha(SUN, st));
        p.circle_stroke(
            centre,
            11.0,
            egui::Stroke::new(2.0, alpha(theme::BRAND, st)),
        );
        label(
            &p,
            centre + vec2(0.0, 14.0),
            egui::Align2::CENTER_TOP,
            "Sun · you",
            theme::SIZE_BODY,
            alpha(theme::CREAM, st),
        );
        let named = if px_per_ly > 22.0 {
            NEAR_STARS.len()
        } else {
            8
        };
        for (i, s) in NEAR_STARS.iter().enumerate() {
            let q = at(s);
            if !screen.expand(40.0).contains(q) {
                continue;
            }
            let r = if s.members > 1 { 3.5 } else { 2.5 };
            p.circle_filled(q, r, alpha(theme::CREAM, st));
            if i < named {
                label(
                    &p,
                    q + vec2(5.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    &format!("{} · {:.1} ly", s.name, s.light_years),
                    theme::SIZE_TINY,
                    alpha(ink, st),
                );
            }
        }
        label(
            &p,
            pos2(screen.left() + 16.0, screen.bottom() - 16.0),
            egui::Align2::LEFT_BOTTOM,
            "Star systems within 16 light years, on the plane of the celestial equator · HYG v3.8",
            theme::SIZE_SUB,
            alpha(theme::TEXT_DIM, st),
        );
    }

    // ── The Milky Way, schematic ────────────────────────────────────────────
    let gx = fade_in(alt, 5.0e17, 3.0e18) * backdrop;
    if gx > 0.01 {
        if galaxy.is_empty() {
            *galaxy = galaxy_points();
        }
        let px_per_ly = (f64::from(focal) / (alt / LIGHT_YEAR_M)) as f32;
        // The Sun in the middle as the board arrives; the galaxy's own middle
        // once the whole of it fits.
        let settle = fade_in(alt, 1.0e19, 4.0e20);
        let gc_from_sun = vec2(0.0, -(SUN_GALACTIC_LY as f32)) * px_per_ly;
        let gc = centre + gc_from_sun * (1.0 - settle);
        let sun_at = gc - gc_from_sun;
        p.circle_filled(
            gc,
            (3_000.0 * px_per_ly).max(2.0),
            alpha(Color32::from_rgb(0xFF, 0xE2, 0xAA), gx * 0.35),
        );
        for (pt, b) in galaxy.iter() {
            let q = gc + vec2(pt.x, pt.y) * px_per_ly;
            if screen.contains(q) {
                p.circle_filled(q, 1.1, alpha(Color32::from_rgb(0xD8, 0xE2, 0xFF), gx * b));
            }
        }
        p.circle_stroke(sun_at, 8.0, egui::Stroke::new(2.0, alpha(theme::BRAND, gx)));
        label(
            &p,
            sun_at + vec2(12.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "Sun · you",
            theme::SIZE_BODY,
            alpha(theme::CREAM, gx),
        );
        label(
            &p,
            gc + vec2(0.0, -14.0),
            egui::Align2::CENTER_BOTTOM,
            "Galactic centre · 26,000 light years",
            theme::SIZE_SUB,
            alpha(ink, gx),
        );
        label(
            &p,
            pos2(screen.left() + 16.0, screen.bottom() - 16.0),
            egui::Align2::LEFT_BOTTOM,
            "The Milky Way from above · schematic: four arms on a log spiral, not each arm as mapped",
            theme::SIZE_SUB,
            alpha(theme::TEXT_DIM, gx),
        );
    }
}

/// The schematic galaxy: points along four logarithmic spiral arms (pitch 12°)
/// and a central bar, light years from the centre, each with a brightness. A
/// fixed seed, so it is the same galaxy every time.
fn galaxy_points() -> Vec<(Vec2, f32)> {
    let mut seed: u32 = 0x9E37_79B9;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32) / (u32::MAX as f32)
    };
    let pitch = 12f32.to_radians().tan();
    let mut out = Vec::with_capacity(4_400);
    for arm in 0..4 {
        let offset = std::f32::consts::FRAC_PI_2 * arm as f32;
        for _ in 0..1_000 {
            let t = rnd() * 2.6;
            let r = 7_000.0 * (pitch * t * 3.0).exp();
            if r > 52_000.0 {
                continue;
            }
            let a = offset + t * 3.0 + (rnd() - 0.5) * 0.25;
            let spread = 1_200.0 + r * 0.05;
            let x = a.cos() * r + (rnd() - 0.5) * spread;
            let y = a.sin() * r + (rnd() - 0.5) * spread;
            out.push((Vec2::new(x, y), 0.3 + 0.6 * rnd()));
        }
    }
    // The bar, about 27,000 light years long, tilted to the Sun's line.
    for _ in 0..400 {
        let along = (rnd() - 0.5) * 27_000.0;
        let across = (rnd() - 0.5) * 4_000.0;
        let (s, c) = 0.45f32.sin_cos();
        out.push((
            Vec2::new(c * along - s * across, s * along + c * across),
            0.5 + 0.4 * rnd(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_read_in_the_unit_that_fits() {
        assert_eq!(distance_words(MOON_DIST_M), "384,400 km");
        assert_eq!(distance_words(1.52 * AU_M), "1.52 AU");
        assert_eq!(distance_words(4.24 * LIGHT_YEAR_M), "4.2 light years");
        assert_eq!(light_time_words(MOON_DIST_M), "1.3 light-seconds");
        assert_eq!(light_time_words(AU_M), "8 min of light");
    }

    #[test]
    fn the_boards_hand_over_without_a_gap() {
        // At every height past the ceiling at least one board is mostly drawn.
        let mut alt = 1.0e8;
        while alt < 1.0e21 {
            let em = fade_out(alt, 1.5e10, 6.0e10);
            let ss = fade_in(alt, 1.0e10, 4.0e10) * fade_out(alt, 1.0e14, 5.0e14);
            let st = fade_in(alt, 1.0e14, 1.0e15) * fade_out(alt, 1.0e18, 5.0e18);
            let gx = fade_in(alt, 5.0e17, 3.0e18);
            assert!(em.max(ss).max(st).max(gx) > 0.45, "nothing at {alt:e}");
            alt *= 1.5;
        }
    }
}
