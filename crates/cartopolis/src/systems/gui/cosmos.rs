//! What lies past the globe's ceiling — the Earth and the Moon, the planets, the
//! nearest stars, the Milky Way, the galaxies round it and the edge of the
//! observable universe — as **one map at one scale**
//! (`docs/mockups/zoom-ladder/`). Past the ceiling the 3D camera waits over the
//! place it left and the zoom moves `CameraState::cosmos_alt` instead; this paints
//! what is out there, behind every panel.
//!
//! **To scale in the middle, squeezed towards the edge.** Every body sits at its
//! real place in one frame (the ecliptic, metres from the Sun) and is projected
//! straight onto the screen; a distance from the middle of the screen then goes
//! through `c·ln(1 + r/c)`, which leaves the middle to scale and gives each
//! further ten-fold the same width of screen towards the edge. So the zoom is
//! one continuous movement — nothing swaps board — and the next thing out is
//! always already at the rim: the planets around the Moon's orbit, the nearest
//! stars around Neptune's, the Milky Way around them, the galaxies around it, the
//! edge of the observable universe around those. Rings at round light-travel
//! times are the ruler. Right at the ceiling the squeeze is off; it eases over
//! the Milky Way so the spiral reads as one; and it lets go at the top of the
//! rail, where the universe is drawn to scale with everything the surveys have
//! mapped a small patch in its middle.
//!
//! **The Earth stays the 3D globe** while it is more than a speck: the camera
//! keeps waiting at the ceiling, and its picture is widened round the globe (a
//! `SubCameraView` larger than the whole image) by as much as the Earth has
//! shrunk, and shifted to where the Earth is on this map; space is painted
//! round it, with a hole. Below two pixels across a painted disc takes over.
//!
//! **A drag moves the map**: the point under the hand follows it, read back
//! through the squeeze (`View::under`), so anything on screen can be pulled to
//! the middle and zoomed into. The Earth is held on screen, and the drag is
//! let go of again just past the ceiling so the way down lands home.
//!
//! **Real bodies at real places.** The Earth lit by the real Sun; the Moon at the
//! sky code's longitude (`utils::sun`), on the ecliptic; the planets where JPL's
//! elements put them today (`utils::planets`); the systems within 16 light years
//! (`utils::near_stars`, HYG); the Milky Way's arms where the parallaxes of
//! young stars put them (Reid et al. 2019), its bar and its disc, in the real
//! galactic plane with its centre in Sagittarius 26,600 light years away; the
//! galaxies from three catalogues atlas joins into one file (`GalaxyCatalogue`, fetched when the view nears them). The view
//! turns as it climbs: from the camera's own direction, to above the ecliptic,
//! to above the galactic plane, to above the supergalactic one; and its middle
//! moves from the Earth to the Sun to the galactic centre — each only once the
//! next one in is too small to lose.
//!
//! Painted with egui on the background layer, before the panel chain, so the
//! panels and the map's chrome stay on top.

use crate::systems::egui_manager::EguiRef;
use crate::systems::theme;
use crate::utils::near_stars::NEAR_STARS;
use crate::utils::planets::{self, AU_M, LIGHT_M_S, LIGHT_YEAR_M, PLANETS};
use bevy::math::{DMat3, DQuat, DVec2, DVec3};
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};
use cartopolis_geo::formats::galaxies::{Catalogue, Galaxies};
use egui::{pos2, vec2, Color32, Pos2};

/// The Earth's radius, metres.
const EARTH_R_M: f64 = 6_371_000.0;
/// The Moon's.
const MOON_R_M: f64 = 1_737_400.0;
/// The Moon's mean distance, metres.
const MOON_DIST_M: f64 = 384_400_000.0;

/// Deep space behind it all.
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
    } else if m < 1.0e4 * LIGHT_YEAR_M {
        format!("{:.1} light years", m / LIGHT_YEAR_M)
    } else if m < 1.0e6 * LIGHT_YEAR_M {
        let ly = (m / LIGHT_YEAR_M / 1_000.0).round() as u64 * 1_000;
        format!("{} light years", thousands(ly))
    } else if m < 1.0e9 * LIGHT_YEAR_M {
        format!("{:.1} million light years", m / LIGHT_YEAR_M / 1.0e6)
    } else {
        format!("{:.1} billion light years", m / LIGHT_YEAR_M / 1.0e9)
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

/// The Sun's radius, metres.
const SUN_R_M: f64 = 696_000_000.0;
/// The parsec, metres.
const PC_M: f64 = 3.085_677_581_491_367e16;
/// The tilt of the Earth's axis to its orbit, radians (23.439°).
const OBLIQUITY: f64 = 0.409_092_6;
/// Equatorial (J2000) to galactic: the rows are the galactic axes in equatorial
/// coordinates, x towards the galactic centre and z the north galactic pole.
const EQ_TO_GAL: [[f64; 3]; 3] = [
    [-0.054_875_560_4, -0.873_437_090_2, -0.483_835_015_5],
    [0.494_109_427_9, -0.444_829_630_0, 0.746_982_244_5],
    [-0.867_666_149_0, -0.198_076_373_4, 0.455_983_776_2],
];
/// The squeeze at its strongest: the radius of the to-scale middle, as a share
/// of half the screen's height. Each further ten-fold then takes
/// `SQUEEZE · ln 10` of it, so half a screen holds about three.
const SQUEEZE: f64 = 0.14;
/// The squeeze is off just past the ceiling, where the Earth drawn here has to
/// be the size of the globe it replaces, and fully on by the Moon's orbit.
const SQUEEZE_FROM_M: f64 = 4.5e7;
const SQUEEZE_TO_M: f64 = 2.0e9;
/// The megaparsec, metres.
const MPC_M: f64 = PC_M * 1.0e6;
/// The supergalactic pole in galactic coordinates (de Vaucouleurs: l = 47.37°,
/// b = +6.32°). The nearby galaxies lie in a sheet round that plane, so it is
/// the one the cosmic web is seen from above.
const SG_POLE_L_B: (f64, f64) = (47.37, 6.32);
/// The edge of the observable universe: the comoving distance to the surface
/// the cosmic microwave background was set free from, 46.5 billion light years
/// (Planck 2018 parameters).
const EDGE_M: f64 = 46.5e9 * LIGHT_YEAR_M;
/// The ruler: round light-travel distances.
const RINGS: [(f64, &str); 15] = [
    (LIGHT_M_S, "1 light-second"),
    (LIGHT_M_S * 60.0, "1 light-minute"),
    (LIGHT_M_S * 3_600.0, "1 light-hour"),
    (LIGHT_M_S * 86_400.0, "1 light-day"),
    (LIGHT_YEAR_M, "1 light year"),
    (LIGHT_YEAR_M * 10.0, "10 light years"),
    (LIGHT_YEAR_M * 100.0, "100 light years"),
    (LIGHT_YEAR_M * 1_000.0, "1,000 light years"),
    (LIGHT_YEAR_M * 10_000.0, "10,000 light years"),
    (LIGHT_YEAR_M * 100_000.0, "100,000 light years"),
    (LIGHT_YEAR_M * 1.0e6, "1 million light years"),
    (LIGHT_YEAR_M * 1.0e7, "10 million light years"),
    (LIGHT_YEAR_M * 1.0e8, "100 million light years"),
    (LIGHT_YEAR_M * 1.0e9, "1 billion light years"),
    (LIGHT_YEAR_M * 1.0e10, "10 billion light years"),
];
const GALAXY_DOT: Color32 = Color32::from_rgb(0xF2, 0xE8, 0xD8);
const EDGE: Color32 = Color32::from_rgb(0xE8, 0xB8, 0x8A);
const RING: Color32 = Color32::from_rgba_unmultiplied_const(0x9A, 0xB0, 0xB8, 46);
const GALAXY_STAR: Color32 = Color32::from_rgb(0xD8, 0xE2, 0xFF);
const GALAXY_BULGE: Color32 = Color32::from_rgb(0xFF, 0xE2, 0xAA);

/// Equatorial coordinates to ecliptic ones (both with x at the March equinox).
fn eq_to_ecl(v: DVec3) -> DVec3 {
    let (s, c) = OBLIQUITY.sin_cos();
    DVec3::new(v.x, v.y * c + v.z * s, -v.y * s + v.z * c)
}

/// A direction from galactic longitude and latitude, degrees.
fn galactic(l: f64, b: f64) -> DVec3 {
    let (l, b) = (l.to_radians(), b.to_radians());
    DVec3::new(b.cos() * l.cos(), b.cos() * l.sin(), b.sin())
}

/// Galactic coordinates to ecliptic ones.
fn gal_to_ecl(g: DVec3) -> DVec3 {
    let [x, y, z] = EQ_TO_GAL;
    eq_to_ecl(DVec3::from(x) * g.x + DVec3::from(y) * g.y + DVec3::from(z) * g.z)
}

/// The rotation from the render frame to the ecliptic one, fixed by two
/// directions known in both: the celestial north pole and the Sun.
fn render_to_ecliptic(north_r: DVec3, sun_r: DVec3, sun_e: DVec3) -> DMat3 {
    let basis = |n: DVec3, s: DVec3| {
        let a = n.normalize();
        let b = (s - a * s.dot(a))
            .try_normalize()
            .unwrap_or_else(|| a.any_orthonormal_vector());
        DMat3::from_cols(a, b, a.cross(b))
    };
    let (s, c) = OBLIQUITY.sin_cos();
    basis(DVec3::new(0.0, s, c), sun_e) * basis(north_r, sun_r).transpose()
}

/// A distance from the middle of the screen, pixels, squeezed: unchanged well
/// inside `c`, logarithmic well outside it.
fn squeeze(lin: f64, c: f64) -> f64 {
    c * (lin / c).ln_1p()
}

/// The other way: how far from the middle, before the squeeze, a point drawn
/// `r` pixels out is.
fn unsqueeze(r: f64, c: f64) -> f64 {
    c * (r / c).exp_m1()
}

/// The one projection every body goes through.
struct View {
    /// What the middle of the screen looks at, ecliptic metres from the Sun.
    middle: DVec3,
    right: DVec3,
    up: DVec3,
    /// Towards the viewer.
    back: DVec3,
    px_per_m: f64,
    /// The squeeze's knee, pixels.
    c: f64,
    centre: Pos2,
}

impl View {
    /// Where the camera's frame (ecliptic) has turned to, what it looks at and
    /// how far the squeeze has come, at a height above the Earth.
    fn new(alt: f64, right0: DVec3, up0: DVec3, earth: DVec3, half_h: f64, focal: f64) -> Self {
        let back0 = right0.cross(up0).normalize();
        let right0 = up0.cross(back0).normalize();
        let up0 = back0.cross(right0);
        let q0 = DQuat::from_mat3(&DMat3::from_cols(right0, up0, back0));
        // Up above the ecliptic once the Moon's orbit is the scene; above the
        // galactic plane once the stars are.
        let to_ecliptic = DQuat::from_rotation_arc(back0, DVec3::Z) * q0;
        let q = q0.slerp(to_ecliptic, f64::from(fade_in(alt, 1.0e8, 5.0e9)));
        let to_galaxy = DQuat::from_rotation_arc(q * DVec3::Z, gal_to_ecl(DVec3::Z)) * q;
        let q = q.slerp(to_galaxy, f64::from(fade_in(alt, 3.0e16, 3.0e18)));
        // And above the supergalactic plane once the Local Volume is the
        // scene, its sheet of galaxies seen face on.
        let (l, b) = SG_POLE_L_B;
        let to_sheet = DQuat::from_rotation_arc(q * DVec3::Z, gal_to_ecl(galactic(l, b))) * q;
        let q = q.slerp(to_sheet, f64::from(fade_in(alt, 3.0e22, 3.0e23)));
        // The Earth in the middle, then the Sun once the inner planets are
        // small, then the galactic centre once the whole of it fits.
        let gc = gal_to_ecl(DVec3::X) * GC_LY * LIGHT_YEAR_M;
        let middle = earth * f64::from(fade_out(alt, 2.0e10, 1.0e12))
            + gc * f64::from(fade_in(alt, 1.0e19, 4.0e20));
        let t = f64::from(fade_in(alt, SQUEEZE_FROM_M, SQUEEZE_TO_M));
        let knee = (60f64.ln() + (SQUEEZE.ln() - 60f64.ln()) * t).exp();
        // Over the Milky Way the squeeze eases, so the galaxy reads as a
        // spiral rather than wound into rings round its middle, and closes
        // again as the Local Group comes in at the rim.
        let knee = knee
            * (0.6 / SQUEEZE).powf(f64::from(
                fade_in(alt, 1.5e20, 6.0e20) * fade_out(alt, 2.0e21, 2.0e22),
            ));
        // Nothing lies past the edge of the observable universe, so at the top
        // of the rail the squeeze lets go and the universe is drawn to scale:
        // everything the surveys have mapped in its middle, the edge round it.
        let knee = knee * (8.0 / SQUEEZE).powf(f64::from(fade_in(alt, 1.0e25, 1.0e27)));
        Self {
            middle,
            right: q * DVec3::X,
            up: q * DVec3::Y,
            back: q * DVec3::Z,
            px_per_m: focal / (alt + EARTH_R_M),
            c: knee * half_h,
            centre: Pos2::ZERO,
        }
    }

    fn at(&self, p: DVec3) -> Pos2 {
        let d = p - self.middle;
        let v = DVec2::new(d.dot(self.right), d.dot(self.up)) * self.px_per_m;
        let lin = v.length();
        if lin < 1e-9 {
            return self.centre;
        }
        let k = squeeze(lin, self.c) / lin;
        pos2(
            self.centre.x + (v.x * k) as f32,
            self.centre.y - (v.y * k) as f32,
        )
    }

    /// A body's radius on screen: to scale, shrunk as the squeeze shrinks the
    /// map where it stands.
    fn size(&self, p: DVec3, r_m: f64) -> f32 {
        (r_m * self.px_per_m / self.shrink(p)) as f32
    }

    /// How much the squeeze shrinks things where `p` stands: 1 in the middle.
    fn shrink(&self, p: DVec3) -> f64 {
        1.0 + self.plane(p - self.middle).length() * self.px_per_m / self.c
    }

    /// An offset in the view's plane, metres right and up.
    fn plane(&self, d: DVec3) -> DVec2 {
        DVec2::new(d.dot(self.right), d.dot(self.up))
    }

    /// Where the view's plane is under a point on screen, metres right and up
    /// of the middle: the inverse of [`Self::at`].
    fn under(&self, s: Pos2) -> DVec2 {
        let v = DVec2::new(
            f64::from(s.x - self.centre.x),
            f64::from(self.centre.y - s.y),
        );
        let r = v.length();
        if r < 1e-9 {
            return DVec2::ZERO;
        }
        v * (unsqueeze(r, self.c) / r / self.px_per_m)
    }

    /// A ring's radius round the middle, pixels.
    fn ring(&self, m: f64) -> f32 {
        squeeze(m * self.px_per_m, self.c) as f32
    }

    /// The Sun's direction on screen from a body, and how far it stands
    /// towards the viewer — what `paint_lit_sphere` takes.
    fn sunlight(&self, body: DVec3) -> (Vec2, f32) {
        let s = (-body).normalize_or_zero();
        (
            Vec2::new(s.dot(self.right) as f32, s.dot(self.up) as f32),
            s.dot(self.back) as f32,
        )
    }
}

/// 1 once two points on screen are `hi` apart, 0 within `lo`: how a body fades
/// as it closes on the one it circles.
fn apart(a: Pos2, b: Pos2, lo: f32, hi: f32) -> f32 {
    let t = ((a.distance(b) - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A label waiting for room: the ones placed first win.
struct Tag {
    at: Pos2,
    align: egui::Align2,
    text: String,
    size: f32,
    ink: Color32,
}

fn place_tags(p: &egui::Painter, tags: Vec<Tag>, screen: egui::Rect) {
    let mut taken: Vec<egui::Rect> = Vec::new();
    for t in tags {
        if t.ink.a() < 4 {
            continue;
        }
        let g = p.layout_no_wrap(t.text.clone(), egui::FontId::proportional(t.size), t.ink);
        let r = t.align.anchor_size(t.at, g.size()).expand(2.0);
        if !screen.contains_rect(r) || taken.iter().any(|o| o.intersects(r)) {
            continue;
        }
        taken.push(r);
        label(p, t.at, t.align, &t.text, t.size, t.ink);
    }
}

/// A circle in the ecliptic plane round `about`, through the view.
fn ecliptic_circle(view: &View, about: DVec3, r: f64) -> Vec<Pos2> {
    (0..=128)
        .map(|i| {
            let a = std::f64::consts::TAU * f64::from(i) / 128.0;
            view.at(about + DVec3::new(a.cos(), a.sin(), 0.0) * r)
        })
        .collect()
}

/// Space over the whole screen but a round hole, where the 3D globe shows
/// through: the four bands round the hole's square, then the ring between the
/// square and the circle, its rays meeting the square's corners exactly.
fn fill_around(p: &egui::Painter, screen: egui::Rect, c: Pos2, r: f32, ink: Color32) {
    let square = egui::Rect::from_center_size(c, vec2(2.0 * r, 2.0 * r));
    if !screen.intersects(square) {
        p.rect_filled(screen, 0.0, ink);
        return;
    }
    for band in [
        egui::Rect::from_min_max(screen.min, pos2(screen.max.x, square.min.y)),
        egui::Rect::from_min_max(pos2(screen.min.x, square.max.y), screen.max),
        egui::Rect::from_min_max(
            pos2(screen.min.x, square.min.y),
            pos2(square.min.x, square.max.y),
        ),
        egui::Rect::from_min_max(
            pos2(square.max.x, square.min.y),
            pos2(screen.max.x, square.max.y),
        ),
    ] {
        let band = band.intersect(screen);
        if band.width() > 0.0 && band.height() > 0.0 {
            p.rect_filled(band, 0.0, ink);
        }
    }
    let mut mesh = egui::Mesh::default();
    // A multiple of eight, so a ray falls on each corner.
    let n = 64;
    for i in 0..=n {
        let a = std::f32::consts::TAU * i as f32 / n as f32;
        let d = Vec2::from_angle(a);
        let edge = d / d.x.abs().max(d.y.abs());
        mesh.colored_vertex(c + vec2(d.x, d.y) * r, ink);
        mesh.colored_vertex(c + vec2(edge.x, edge.y) * r, ink);
        if i > 0 {
            let k = 2 * i as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    p.add(egui::Shape::mesh(mesh));
}

/// Paint what is past the ceiling, while the view is out there, and frame the
/// 3D globe to the size and place the Earth has there.
#[allow(clippy::too_many_arguments)]
pub fn render_cosmos(
    egui: EguiRef,
    mut camera_state: ResMut<crate::resources::CameraState>,
    dev: Res<crate::resources::DevPanelState>,
    clock: Res<crate::systems::transit_live::WorldClock>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<&Transform, With<crate::components::FreeCamera>>,
    mut camera_lens: Query<(&Projection, &mut Camera), With<crate::components::FreeCamera>>,
    mut galaxy: Local<Vec<(DVec3, f32, Color32)>>,
    catalogue: Res<GalaxyCatalogue>,
) {
    let alt = camera_state.cosmos_alt;
    let Ok((projection, mut lens)) = camera_lens.single_mut() else {
        return;
    };
    if alt <= 0.0 {
        camera_state.cosmos_drag = Vec2::ZERO;
        if lens.sub_camera_view.is_some() {
            lens.sub_camera_view = None;
        }
        return;
    }
    let Ok(tf) = camera.single() else {
        return;
    };
    let ctx = egui.ctx();
    let screen = ctx.content_rect();
    let p = ctx.layer_painter(egui::LayerId::background());
    // Where the 3D camera waits, from the Earth's centre, and its height there.
    let wait_m = crate::systems::free_camera::ORBIT_MAX_DISTANCE_SPACE;
    let ceiling = wait_m - f64::from(globe.radius);
    let bd = fade_in(alt, ceiling, ceiling * 1.5);

    let fov = match projection {
        Projection::Perspective(pp) => pp.fov,
        _ => std::f32::consts::FRAC_PI_4,
    };
    let half_h = f64::from(screen.height() * 0.5);
    let focal = half_h / f64::from((fov * 0.5).tan());
    let now = clock.now_unix;

    // The real Sun and Moon, whatever the light setting holds the street at,
    // and the frame they and the camera are in turned onto the ecliptic.
    let hour = crate::utils::sun::solar_time_of_day(now, globe.anchor_lng);
    let sun_r = crate::utils::sun::sun_direction(hour, dev.sun_declination_rad).as_dvec3();
    let age = crate::utils::sun::lunar_age_days(now);
    let moon_r = crate::utils::sun::moon_direction(hour, age).as_dvec3();
    let earth = DVec3::from(planets::position(&PLANETS[planets::EARTH], now)) * AU_M;
    let to_ecl = render_to_ecliptic(
        globe.north_axis_local().as_dvec3(),
        sun_r,
        -earth.normalize(),
    );
    let mut view = View::new(
        alt,
        to_ecl * (tf.rotation * Vec3::X).as_dvec3(),
        to_ecl * (tf.rotation * Vec3::Y).as_dvec3(),
        earth,
        half_h,
        focal,
    );
    view.centre = screen.center();

    // ── Moving about: a drag carries the point under the hand with it ──────
    // Read through the squeeze, so a drag from the rim to the middle brings
    // what was at the rim to the middle, however far out it is.
    let drag = std::mem::take(&mut camera_state.cosmos_drag) / ctx.zoom_factor();
    let mut pan = view.plane(camera_state.cosmos_pan);
    if drag != Vec2::ZERO {
        let at = ctx
            .input(|i| i.pointer.latest_pos())
            .filter(|at| screen.contains(*at))
            .unwrap_or(view.centre);
        pan += view.under(at - vec2(drag.x, drag.y)) - view.under(at);
    }
    // Held so the Earth stays on screen, and let go of entirely just past
    // the ceiling, where the globe takes the view back from the middle.
    let home = view.plane(earth - view.middle);
    let room = f64::from(screen.width().min(screen.height())) * 0.4;
    let reach = (unsqueeze(room, view.c) / view.px_per_m)
        * f64::from(fade_in(alt, ceiling * 1.05, ceiling * 2.0));
    let off = home - pan;
    let limit = reach.max(home.length());
    if off.length() > limit {
        pan = home - off.normalize() * limit;
    }
    camera_state.cosmos_pan = view.right * pan.x + view.up * pan.y;
    view.middle += camera_state.cosmos_pan;

    // ── The Earth is the 3D globe while it is more than a speck ─────────────
    // The camera stays where it waits; its picture is widened round the
    // globe instead (a sub-view larger than the whole), by as much as the
    // Earth has shrunk here, and moved to where the Earth is drawn.
    let earth_at = view.at(earth);
    let wide = ((alt + f64::from(globe.radius)) / wait_m * view.shrink(earth)).clamp(1.0, 1.0e4);
    let globe_r = (focal * f64::from(globe.radius)
        / (wait_m * wait_m - f64::from(globe.radius).powi(2)).sqrt()
        / wide) as f32;
    let solid = globe_r >= 2.0;
    let sub = lens.physical_target_size().filter(|_| solid).map(|full| {
        let ppp = ctx.pixels_per_point();
        let e = (earth_at - screen.min) * ppp;
        let size = full.as_dvec2() * wide;
        bevy::camera::SubCameraView {
            full_size: full,
            offset: Vec2::new(
                (f64::from(full.x) * 0.5 - f64::from(e.x) * wide) as f32,
                (f64::from(full.y) * 0.5 - f64::from(e.y) * wide) as f32,
            ),
            size: UVec2::new(size.x.round() as u32, size.y.round() as u32),
        }
    });
    if lens.sub_camera_view != sub {
        lens.sub_camera_view = sub;
    }
    if solid {
        // A little past the limb, for the atmosphere's rim.
        fill_around(&p, screen, earth_at, globe_r * 1.05, alpha(SPACE, bd));
    } else {
        p.rect_filled(screen, 0.0, alpha(SPACE, bd));
    }
    let mut tags: Vec<Tag> = Vec::new();

    // ── The ruler ───────────────────────────────────────────────────────────
    let ruler = bd * fade_in(alt, 1.0e8, SQUEEZE_TO_M);
    let reach = screen.width().hypot(screen.height()) * 0.5;
    let mut ring_tags = Vec::new();
    for (m, words) in RINGS {
        let r = view.ring(m);
        let a = ruler
            * crate::utils::sun::smoothstep(24.0, 64.0, r)
            * (1.0 - crate::utils::sun::smoothstep(reach * 0.9, reach, r));
        if a > 0.01 {
            p.circle_stroke(view.centre, r, egui::Stroke::new(1.0, alpha(RING, a)));
            ring_tags.push(Tag {
                at: view.centre + vec2(r, -r) * std::f32::consts::FRAC_1_SQRT_2 + vec2(3.0, -2.0),
                align: egui::Align2::LEFT_BOTTOM,
                text: words.to_string(),
                size: theme::SIZE_TINY,
                ink: alpha(theme::TEXT_DIM, a),
            });
        }
    }

    // ── The Milky Way, its arms where they were measured ────────────────────
    let sun_at = view.at(DVec3::ZERO);
    let gx = bd * fade_in(alt, 3.0e17, 3.0e18);
    if gx > 0.01 {
        if galaxy.is_empty() {
            *galaxy = galaxy_points()
                .into_iter()
                .map(|(pt, b, ink)| {
                    let g = DVec3::new(GC_LY - f64::from(pt.y), f64::from(pt.x), 0.0);
                    (gal_to_ecl(g) * LIGHT_YEAR_M, b, ink)
                })
                .collect();
        }
        let gc = gal_to_ecl(DVec3::X) * GC_LY * LIGHT_YEAR_M;
        let gc_at = view.at(gc);
        // Once the whole galaxy is a few pixels its points are one dot.
        let mw_r = view.size(gc, 50_000.0 * LIGHT_YEAR_M);
        if mw_r > 4.0 {
            // One mesh of small squares: some 7,000 circles a frame was a
            // tessellation's worth of work for something this small.
            let mut mesh = egui::Mesh::default();
            let r = 0.75;
            for (pt, b, ink) in galaxy.iter() {
                let q = view.at(*pt);
                if !screen.contains(q) {
                    continue;
                }
                let ink = alpha(*ink, gx * b);
                let i = mesh.vertices.len() as u32;
                for d in [vec2(-r, -r), vec2(r, -r), vec2(r, r), vec2(-r, r)] {
                    mesh.colored_vertex(q + d, ink);
                }
                mesh.add_triangle(i, i + 1, i + 2);
                mesh.add_triangle(i, i + 2, i + 3);
            }
            p.add(egui::Shape::mesh(mesh));
        } else {
            p.circle_filled(gc_at, mw_r.max(2.5), alpha(GALAXY_STAR, gx));
        }
        tags.push(Tag {
            at: gc_at + vec2(0.0, -14.0),
            align: egui::Align2::CENTER_BOTTOM,
            text: format!("Galactic centre · {} light years", thousands(GC_LY as u64)),
            size: theme::SIZE_SUB,
            ink: alpha(
                theme::TEXT,
                gx * apart(gc_at, sun_at, 30.0, 80.0) * fade_out(alt, 2.0e21, 6.0e21),
            ),
        });
        tags.push(Tag {
            at: gc_at + vec2(0.0, mw_r.max(2.5) + 8.0),
            align: egui::Align2::CENTER_TOP,
            text: "Milky Way · you".into(),
            size: theme::SIZE_BODY,
            ink: alpha(theme::CREAM, gx * fade_in(alt, 2.0e21, 6.0e21)),
        });
    }

    // ── The galaxies: the Local Group, the Local Volume, the cosmic web ─────
    // By distance, not by catalogue: Andromeda is the Local Volume
    // catalogue's but comes in with the Local Group.
    let (near_a, volume_a, far_a) = (
        bd * fade_in(alt, 1.0e20, 6.0e20),
        bd * fade_in(alt, 6.0e20, 4.0e21),
        bd * fade_in(alt, 4.0e21, 3.0e22),
    );
    // A nearby galaxy comes in by its own distance: the view is flat, and one
    // 30 million light years off straight above the Milky Way would otherwise
    // be drawn among the dwarfs a hundred times nearer. The survey comes in as
    // one, at the rim.
    let fade = |g: &PlacedGalaxy| {
        let d = g.light_years * LIGHT_YEAR_M;
        if g.light_years < 3.6e7 {
            bd * fade_in(alt, d * 0.03, d * 0.2)
        } else {
            far_a
        }
    };
    // Kept off the Milky Way while it is a picture of its own; past the
    // nearby galaxies the whole survey is a patch round it, and is drawn.
    let keep_off = fade_out(alt, 3.0e23, 3.0e24);
    // Fewer names on a narrow screen.
    let names = if screen.width() < 600.0 { 12 } else { 30 };
    let mut galaxy_tags = Vec::new();
    if !catalogue.galaxies.is_empty() && near_a > 0.01 {
        let mut named = paint_galaxies(
            &p,
            &view,
            screen,
            &catalogue.galaxies,
            fade,
            sun_at,
            keep_off,
        );
        // The clusters first, then the brightest in our sky (luminosity over
        // distance squared, so Andromeda before a brighter galaxy five times
        // farther); a label's room goes to the first that asks.
        let seen = |g: &PlacedGalaxy| f64::from(g.log_lum) - 2.0 * g.light_years.max(1.0).log10();
        named.sort_by(|a, b| {
            let cluster =
                |g: &PlacedGalaxy| g.name.contains("Cluster") || g.name.contains("Supercluster");
            cluster(b.0)
                .cmp(&cluster(a.0))
                .then(seen(b.0).total_cmp(&seen(a.0)))
        });
        for (g, q, a) in named.into_iter().take(names) {
            galaxy_tags.push(Tag {
                at: q + vec2(5.0, 0.0),
                align: egui::Align2::LEFT_CENTER,
                text: format!(
                    "{} · {}",
                    g.name,
                    distance_words(g.light_years * LIGHT_YEAR_M)
                ),
                size: theme::SIZE_TINY,
                // At the top the whole survey is a dot; its names would bury it.
                ink: alpha(theme::TEXT, a * fade_out(alt, 1.0e26, 6.0e26)),
            });
        }
    }

    // ── The edge of the observable universe ─────────────────────────────────
    let edge_r = view.ring(EDGE_M);
    let ea = bd * crate::utils::sun::smoothstep(reach * 1.4, reach * 0.9, edge_r);
    if ea > 0.01 {
        p.circle_stroke(view.centre, edge_r, egui::Stroke::new(1.5, alpha(EDGE, ea)));
        tags.push(Tag {
            at: view.centre + vec2(0.0, -edge_r - 6.0),
            align: egui::Align2::CENTER_BOTTOM,
            text: "Edge of the observable universe · 46 billion light years".into(),
            size: theme::SIZE_SUB,
            ink: alpha(EDGE, ea),
        });
        // What the survey covers, once it is a patch in the middle.
        let mapped = view.ring(2.0e9 * LIGHT_YEAR_M);
        tags.push(Tag {
            // Above the middle's own "Milky Way · you".
            at: view.centre + vec2(mapped + 6.0, -6.0),
            align: egui::Align2::LEFT_BOTTOM,
            text: "every galaxy mapped here · 2 billion light years".into(),
            size: theme::SIZE_TINY,
            ink: alpha(theme::TEXT, ea * fade_in(alt, 3.0e25, 3.0e26)),
        });
    }

    // ── The nearest stars ───────────────────────────────────────────────────
    let star_at: Vec<Pos2> = NEAR_STARS
        .iter()
        .map(|s| view.at(eq_to_ecl(DVec3::from(s.xyz_pc.map(f64::from))) * PC_M))
        .collect();
    let near = |i: usize| {
        let a = &NEAR_STARS[i];
        (0..NEAR_STARS.len()).filter(|j| *j != i).min_by(|x, y| {
            let d = |s: &crate::utils::near_stars::NearStar| {
                (0..3)
                    .map(|k| (s.xyz_pc[k] - a.xyz_pc[k]).powi(2))
                    .sum::<f32>()
            };
            d(&NEAR_STARS[*x]).total_cmp(&d(&NEAR_STARS[*y]))
        })
    };
    let mut star_tags = Vec::new();
    for (i, s) in NEAR_STARS.iter().enumerate() {
        let q = star_at[i];
        if !screen.expand(200.0).contains(q) {
            continue;
        }
        let a = bd * apart(q, sun_at, 4.0, 18.0);
        if a < 0.01 {
            continue;
        }
        // A lane runs only between two stars on screen: one to a star far off
        // past the rim crosses the whole picture and leads nowhere visible.
        if let Some(j) = near(i).filter(|j| screen.contains(q) && screen.contains(star_at[*j])) {
            p.line_segment([q, star_at[j]], egui::Stroke::new(1.0, alpha(LANE, a)));
        }
        if i < 3 && screen.contains(q) {
            p.line_segment([sun_at, q], egui::Stroke::new(1.0, alpha(LANE, a)));
        }
        p.circle_filled(
            q,
            if s.members > 1 { 3.5 } else { 2.5 },
            alpha(theme::CREAM, a),
        );
        star_tags.push(Tag {
            at: q + vec2(5.0, 0.0),
            align: egui::Align2::LEFT_CENTER,
            text: format!("{} · {:.1} ly", s.name, s.light_years),
            size: theme::SIZE_TINY,
            ink: alpha(theme::TEXT, a),
        });
    }

    // ── The Sun and the planets ─────────────────────────────────────────────
    let you_apart = apart(earth_at, sun_at, 6.0, 20.0);
    // An orbit seen edge-on is a line through the screen, not a ring: drawn
    // only once the view has turned to look down on it.
    let face_on = crate::utils::sun::smoothstep(0.6, 0.9, view.back.z.abs() as f32);
    // And the planets' orbits only once the Sun is the middle: the squeeze is
    // round the middle, and a ring running through it (the Earth's own, while
    // the Earth is the middle) comes out as a teardrop.
    let sun_middle = fade_in(alt, 1.0e11, 1.0e12);
    let mut planet_tags = Vec::new();
    for (i, planet) in PLANETS.iter().enumerate() {
        let xyz = DVec3::from(planets::position(planet, now)) * AU_M;
        let q = view.at(xyz);
        let a = bd * apart(q, sun_at, 3.0, 12.0);
        if a < 0.01 {
            continue;
        }
        p.add(egui::Shape::line(
            ecliptic_circle(&view, DVec3::ZERO, xyz.truncate().length()),
            egui::Stroke::new(
                1.0,
                alpha(
                    ORBIT,
                    a * face_on * sun_middle * apart(q, sun_at, 6.0, 30.0),
                ),
            ),
        ));
        if i == planets::EARTH {
            continue;
        }
        p.circle_filled(q, if i >= 4 { 4.5 } else { 3.5 }, alpha(PLANET_TONES[i], a));
        planet_tags.push(Tag {
            at: q + vec2(0.0, 7.0),
            align: egui::Align2::CENTER_TOP,
            text: format!(
                "{} · {}",
                planet.name,
                light_time_words(xyz.distance(earth))
            ),
            size: theme::SIZE_SUB,
            ink: alpha(theme::CREAM, a),
        });
    }
    let sun_r = view.size(DVec3::ZERO, SUN_R_M).max(5.0);
    p.circle_filled(sun_at, sun_r * 1.8, alpha(SUN, bd * 0.25));
    p.circle_filled(sun_at, sun_r, alpha(SUN, bd));

    // ── The Earth and the Moon, to scale ────────────────────────────────────
    let moon_ecl = to_ecl * moon_r;
    let moon = earth
        + DVec3::new(moon_ecl.x, moon_ecl.y, 0.0)
            .try_normalize()
            .unwrap_or(DVec3::X)
            * MOON_DIST_M;
    let moon_at = view.at(moon);
    let ma = bd * apart(moon_at, earth_at, 4.0, 14.0);
    if ma > 0.01 {
        p.add(egui::Shape::line(
            ecliptic_circle(&view, earth, MOON_DIST_M),
            egui::Stroke::new(
                1.0,
                alpha(ORBIT, ma * face_on * apart(moon_at, earth_at, 8.0, 40.0)),
            ),
        ));
    }
    let earth_r = if solid {
        globe_r
    } else {
        let earth_r = view.size(earth, EARTH_R_M).max(2.5);
        let (light, toward) = view.sunlight(earth);
        paint_lit_sphere(
            &p,
            earth_at,
            earth_r,
            light,
            toward,
            alpha(EARTH_DAY, bd * you_apart.max(0.3)),
            alpha(EARTH_NIGHT, bd * you_apart.max(0.3)),
        );
        earth_r
    };
    if ma > 0.01 {
        let (light, toward) = view.sunlight(moon);
        paint_lit_sphere(
            &p,
            moon_at,
            view.size(moon, MOON_R_M).max(2.0),
            light,
            toward,
            alpha(MOON_DAY, ma),
            alpha(MOON_NIGHT, ma),
        );
    }
    // Home: the ring stays on the Earth, wherever the Earth has shrunk to.
    p.circle_stroke(
        earth_at,
        earth_r + 6.0,
        egui::Stroke::new(2.0, alpha(theme::BRAND, bd)),
    );

    // ── Labels, the nearer first ────────────────────────────────────────────
    let sun_words = if you_apart < 0.5 {
        "Sun · you".to_string()
    } else {
        format!("Sun · {}", light_time_words(earth.length()))
    };
    let mut ordered = vec![
        Tag {
            at: earth_at + vec2(0.0, earth_r + 10.0),
            align: egui::Align2::CENTER_TOP,
            text: "Earth · you".into(),
            size: theme::SIZE_BODY,
            ink: alpha(theme::CREAM, bd * you_apart),
        },
        Tag {
            at: sun_at + vec2(0.0, sun_r + 10.0),
            align: egui::Align2::CENTER_TOP,
            text: sun_words,
            size: theme::SIZE_BODY,
            ink: alpha(SUN, bd * fade_out(alt, 2.0e21, 6.0e21)),
        },
        Tag {
            at: moon_at + vec2(8.0, 0.0),
            align: egui::Align2::LEFT_CENTER,
            text: format!(
                "Moon · {} · {}",
                distance_words(MOON_DIST_M),
                light_time_words(MOON_DIST_M)
            ),
            size: theme::SIZE_SUB,
            ink: alpha(theme::CREAM, ma),
        },
    ];
    ordered.extend(planet_tags);
    ordered.append(&mut tags);
    ordered.extend(galaxy_tags);
    ordered.extend(star_tags);
    ordered.extend(ring_tags);

    // What the picture is, and where its bodies come from.
    let mut said = Vec::new();
    if fade_in(alt, 1.0e9, 1.0e10) > 0.5 && alt < 3.0e16 {
        said.push("planets where they are today");
    }
    if alt > 1.0e13 && alt < 1.0e19 {
        said.push("stars within 16 light years, HYG v3.8");
    }
    if gx > 0.5 && alt < 3.0e21 {
        said.push("the Milky Way's arms as measured, Reid et al. 2019; its far side carried on");
    }
    let galaxy_words;
    if near_a > 0.5 {
        galaxy_words = match (&catalogue.failed, catalogue.galaxies.is_empty()) {
            (Some(_), _) => "the galaxies could not be fetched".to_string(),
            (None, true) => "fetching the galaxies…".to_string(),
            (None, false) => {
                let mut from = vec!["McConnachie 2012"];
                if volume_a > 0.5 {
                    from.push("UNGC 2013");
                }
                if far_a > 0.5 {
                    // The why of the empty band, where there is room for it.
                    from.push(if screen.width() < 600.0 {
                        "2MRS 2012"
                    } else {
                        "2MRS 2012; the empty band is our own disc in the way"
                    });
                }
                format!("galaxies: {}", from.join(" · "))
            }
        };
        said.push(galaxy_words.as_str());
    }
    // Above the map's bottom bar, which sits over the background layer.
    let foot = pos2(screen.left() + 16.0, screen.bottom() - 64.0);
    if !said.is_empty() {
        label(
            &p,
            foot,
            egui::Align2::LEFT_BOTTOM,
            &{
                let words = said.join(" · ");
                let mut c = words.chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect::<String>())
                    .unwrap_or_default()
            },
            theme::SIZE_SUB,
            alpha(theme::TEXT_DIM, bd),
        );
    }
    label(
        &p,
        foot - vec2(0.0, 18.0),
        egui::Align2::LEFT_BOTTOM,
        "To scale in the middle, squeezed towards the edge · rings at light-travel distances",
        theme::SIZE_SUB,
        alpha(theme::TEXT_DIM, ruler),
    );
    // Clear of the rail on the right and of the captions and the bar below.
    let mut room = screen.shrink2(vec2(48.0, 8.0));
    room.max.y = foot.y - 40.0;
    place_tags(&p, ordered, room);
}

/// One galaxy past the Milky Way, placed in the map's frame.
struct PlacedGalaxy {
    /// Ecliptic metres from the Sun.
    at: DVec3,
    log_lum: f32,
    catalogue: Catalogue,
    name: String,
    light_years: f64,
}

/// The galaxies past the Milky Way (`cosmos/galaxies.bin`, atlas's `galaxies`
/// dataset), fetched once the view climbs towards them and kept.
#[derive(Resource, Default)]
pub struct GalaxyCatalogue {
    task: Option<Task<Result<Galaxies, String>>>,
    galaxies: Vec<PlacedGalaxy>,
    /// Why there are none, once asked and answered without them.
    failed: Option<String>,
}

impl crate::systems::stream_watch::StreamBusy for GalaxyCatalogue {
    const NAME: &'static str = "galaxies";
    fn stream_busy(&self) -> bool {
        self.task.is_some()
    }
}

/// The height the catalogue is fetched from: the Local Group is still past the
/// rim there, so it has arrived by the time it comes in.
const GALAXIES_FROM_M: f64 = 1.0e19;

/// Galaxies drawn at most: the file's order is the thinning (brightest of the
/// redshift survey first), so a phone keeps every galaxy near enough to name
/// and drops the survey's faintest.
const GALAXIES_MAX: usize = if cfg!(any(target_os = "android", target_arch = "wasm32")) {
    12_000
} else {
    50_000
};

async fn fetch_galaxies() -> Result<Galaxies, String> {
    let base = crate::systems::osm_buildings::tdbag_url();
    let url = format!("{}/cosmos/galaxies.bin", base.trim_end_matches('/'));
    let bytes = crate::platform::http::fetch(crate::platform::http::Request::get(url)).await?;
    Galaxies::decode(&bytes).map_err(|e| e.to_string())
}

/// Fetch the catalogue once the view is out far enough, and place it.
pub fn drive_galaxies(
    camera_state: Res<crate::resources::CameraState>,
    mut catalogue: ResMut<GalaxyCatalogue>,
) {
    if let Some(task) = catalogue.task.as_mut() {
        if !task.is_finished() {
            return;
        }
        let Some(answer) = crate::platform::http::poll_task(task) else {
            return;
        };
        catalogue.task = None;
        match answer {
            Ok(doc) => {
                catalogue.galaxies = doc
                    .galaxies
                    .into_iter()
                    .take(GALAXIES_MAX)
                    .map(|g| {
                        let eq = DVec3::from(g.xyz_mpc.map(f64::from)) * MPC_M;
                        PlacedGalaxy {
                            at: eq_to_ecl(eq),
                            log_lum: g.log_lum,
                            catalogue: g.catalogue,
                            light_years: eq.length() / LIGHT_YEAR_M,
                            name: g.name,
                        }
                    })
                    .collect();
                info!("cosmos: {} galaxies", catalogue.galaxies.len());
            }
            Err(e) => {
                warn!("cosmos: no galaxies: {e}");
                catalogue.failed = Some(e);
            }
        }
        return;
    }
    let asked = !catalogue.galaxies.is_empty() || catalogue.failed.is_some();
    if !asked && camera_state.cosmos_alt > GALAXIES_FROM_M {
        catalogue.task = Some(IoTaskPool::get().spawn(fetch_galaxies()));
    }
}

/// A galaxy's dot, pixels: by its luminosity, the Milky Way's own (10¹⁰·⁸ L☉
/// in K) about three across, a dwarf one.
fn galaxy_dot(log_lum: f32) -> f32 {
    (0.6 + (log_lum - 8.0) * 0.45).clamp(0.6, 2.0)
}

/// The galaxies, one mesh of small squares (forty thousand circles would each
/// be their own shape), at most one per two-pixel bin so the survey's far shell,
/// squeezed into the rim, is not drawn over itself many times. Answers the
/// named ones on screen, brightest first, for labels.
#[allow(clippy::too_many_arguments)]
fn paint_galaxies<'a>(
    p: &egui::Painter,
    view: &View,
    screen: egui::Rect,
    galaxies: &'a [PlacedGalaxy],
    fade: impl Fn(&PlacedGalaxy) -> f32,
    away_from: Pos2,
    keep_off: f32,
) -> Vec<(&'a PlacedGalaxy, Pos2, f32)> {
    let (w, h) = (
        (screen.width() / 2.0).ceil() as usize + 1,
        (screen.height() / 2.0).ceil() as usize + 1,
    );
    let mut taken = vec![0u64; (w * h).div_ceil(64)];
    let mut mesh = egui::Mesh::default();
    let mut named = Vec::new();
    for g in galaxies {
        let a = fade(g);
        if a < 0.01 {
            continue;
        }
        let q = view.at(g.at);
        if !screen.contains(q) {
            continue;
        }
        // Not over the Milky Way the view is centred on, while it is drawn.
        let a = a * apart(q, away_from, 4.0, 14.0).max(1.0 - keep_off);
        if a < 0.01 {
            continue;
        }
        let bin =
            ((q.y - screen.top()) / 2.0) as usize * w + ((q.x - screen.left()) / 2.0) as usize;
        let (word, bit) = (bin / 64, 1u64 << (bin % 64));
        if taken[word] & bit != 0 && g.name.is_empty() {
            continue;
        }
        taken[word] |= bit;
        // The survey's 43,000 small and faint, so its clusters and voids show
        // as density rather than as one lit disc; the named nearer ones a size up.
        let (r, ink) = if g.catalogue == Catalogue::Redshift && g.name.is_empty() {
            (galaxy_dot(g.log_lum) * 0.5, 0.3)
        } else {
            (galaxy_dot(g.log_lum).max(1.0), 0.8)
        };
        let ink = alpha(GALAXY_DOT, a * ink);
        let i = mesh.vertices.len() as u32;
        for d in [vec2(-r, -r), vec2(r, -r), vec2(r, r), vec2(-r, r)] {
            mesh.colored_vertex(q + d, ink);
        }
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
        if !g.name.is_empty() {
            named.push((g, q, a));
        }
    }
    p.add(egui::Shape::mesh(mesh));
    named
}

/// The Sun's distance from the galactic centre, light years: the 8.15 kpc the
/// arm model below was fitted with (Reid et al. 2019).
const GC_LY: f64 = 26_600.0;
/// Light years in a kiloparsec.
const LY_PER_KPC: f32 = 3_261.6;

/// The Milky Way's spiral arms as Reid et al. (2019, ApJ 885, 131, table 2)
/// fitted them to the parallaxes of ~200 young massive stars: each a log
/// spiral `ln(R / R_kink) = −(β − β_kink)·tan ψ`, with a different pitch on
/// either side of the kink. β is the azimuth round the centre from the Sun,
/// growing the way the Galaxy turns; R in kpc. Columns: the β range measured
/// (degrees), β_kink, R_kink, ψ below and above the kink (degrees), and the
/// arm's 1σ width at the kink (kpc).
const ARMS: [(f32, f32, f32, f32, f32, f32, f32); 7] = [
    // 3-kpc (near)
    (15.0, 18.0, 15.0, 3.52, -4.2, -4.2, 0.18),
    // Norma
    (5.0, 54.0, 18.0, 4.46, -1.0, 19.5, 0.14),
    // Scutum–Centaurus
    (0.0, 104.0, 23.0, 4.91, 14.1, 12.1, 0.23),
    // Sagittarius–Carina
    (2.0, 97.0, 24.0, 6.04, 17.1, 1.0, 0.27),
    // Local (the Orion spur, the Sun on its inner edge)
    (-8.0, 34.0, 9.0, 8.26, 11.4, 11.4, 0.31),
    // Perseus
    (-23.0, 115.0, 40.0, 8.87, 10.3, 8.7, 0.35),
    // Outer
    (-16.0, 71.0, 18.0, 12.24, 3.0, 9.4, 0.65),
];
/// How far an arm is carried on past the azimuths measured, degrees: the far
/// side of the Galaxy, behind its centre, has no parallaxes yet, and is drawn
/// fainter for it.
const ARM_REACH_DEG: f32 = 150.0;
/// Where H II regions glow along the arms.
const GALAXY_HII: Color32 = Color32::from_rgb(0xFF, 0xA8, 0xC4);

/// The Milky Way as points, light years from its centre in its own plane (y
/// towards the Sun, x the way the Galaxy turns there, towards l = 90°), each
/// with a brightness and a colour: the arms as measured and carried on fainter
/// round the far side; the long bar (Wegg, Gerhard & Portail 2015: 5 kpc half
/// length, 28° from the line to the Sun, its near end at positive longitude)
/// and the boxy bulge in it; and the old disc, exponential with a 2.6 kpc
/// scale length (Bland-Hawthorn & Gerhard 2016). A fixed seed, so it is the
/// same galaxy every time.
fn galaxy_points() -> Vec<(Vec2, f32, Color32)> {
    let mut seed: u32 = 0x9E37_79B9;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32) / (u32::MAX as f32)
    };
    let kpc = |x: f32, y: f32| Vec2::new(x, y) * LY_PER_KPC;
    let mut out = Vec::with_capacity(7_000);

    // The arms: points along each, spread across it by its width.
    for &(lo, hi, kink, r_kink, psi_lo, psi_hi, width) in &ARMS {
        let span = (hi - lo) + 2.0 * ARM_REACH_DEG;
        let n = (span * 6.0) as usize;
        for _ in 0..n {
            let beta = lo - ARM_REACH_DEG + rnd() * span;
            let psi = if beta <= kink { psi_lo } else { psi_hi };
            let r = r_kink * (-(beta - kink).to_radians() * psi.to_radians().tan()).exp();
            // Inside the bar's reach an arm is the bar's; past 17 kpc the
            // disc has thinned out.
            if !(3.0..17.0).contains(&r) {
                continue;
            }
            let beyond = (lo - beta).max(beta - hi).max(0.0) / ARM_REACH_DEG;
            let b = beta.to_radians();
            let centre = kpc(r * b.sin(), r * b.cos());
            // Two uniform draws make a bump close enough to a Gaussian.
            let w = (width + 0.042 * (r - r_kink)).max(0.1);
            let across = (rnd() + rnd() + rnd() - 1.5) * 2.0 * w;
            let at = centre + Vec2::new(b.sin(), b.cos()) * across * LY_PER_KPC;
            let measured = 1.0 - 0.6 * beyond.sqrt();
            let (ink, glow) = if rnd() < 0.07 {
                (GALAXY_HII, 0.9)
            } else {
                (GALAXY_STAR, 0.35 + 0.45 * rnd())
            };
            out.push((at, glow * measured, ink));
        }
    }

    // The long bar and the boxy bulge at its middle.
    let (s, c) = 28f32.to_radians().sin_cos();
    let along_axis = Vec2::new(s, c);
    let across_axis = Vec2::new(c, -s);
    for i in 0..1_400 {
        let (half, thick) = if i < 900 { (5.0, 0.45) } else { (1.6, 0.7) };
        let along = (rnd() + rnd() - 1.0) * half;
        let across = (rnd() + rnd() + rnd() - 1.5) * thick;
        let at = (along_axis * along + across_axis * across) * LY_PER_KPC;
        out.push((at, 0.35 + 0.4 * rnd(), GALAXY_BULGE));
    }

    // The old disc, faint and smooth: surface density falling as e^(−R/2.6
    // kpc), so a radius drawn as the sum of two exponential draws.
    for _ in 0..2_200 {
        let r = -2.6 * (rnd().max(1e-6) * rnd().max(1e-6)).ln();
        if !(2.0..18.0).contains(&r) {
            continue;
        }
        let a = rnd() * std::f32::consts::TAU;
        out.push((
            kpc(r * a.sin(), r * a.cos()),
            0.15 + 0.15 * rnd(),
            GALAXY_BULGE,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_scenario::ladder::Rung;

    #[test]
    fn distances_read_in_the_unit_that_fits() {
        assert_eq!(distance_words(MOON_DIST_M), "384,400 km");
        assert_eq!(distance_words(1.52 * AU_M), "1.52 AU");
        assert_eq!(distance_words(4.24 * LIGHT_YEAR_M), "4.2 light years");
        assert_eq!(
            distance_words(163_000.0 * LIGHT_YEAR_M),
            "163,000 light years"
        );
        assert_eq!(
            distance_words(2.54e6 * LIGHT_YEAR_M),
            "2.5 million light years"
        );
        assert_eq!(
            distance_words(4.65e10 * LIGHT_YEAR_M),
            "46.5 billion light years"
        );
        assert_eq!(light_time_words(MOON_DIST_M), "1.3 light-seconds");
        assert_eq!(light_time_words(AU_M), "8 min of light");
    }

    #[test]
    fn the_galactic_centre_is_in_sagittarius() {
        let [x, ..] = EQ_TO_GAL;
        let v = DVec3::from(x);
        let ra = v.y.atan2(v.x).to_degrees().rem_euclid(360.0);
        let dec = v.z.asin().to_degrees();
        assert!(
            (ra - 266.40).abs() < 0.1 && (dec + 28.94).abs() < 0.1,
            "{ra} {dec}"
        );
    }

    #[test]
    fn the_frame_turns_the_sun_and_the_pole_onto_the_ecliptic() {
        let north_r = DVec3::new(0.0, 0.79, -0.61).normalize();
        let sun_r = DVec3::new(0.3, 0.6, 0.74).normalize();
        let sun_e = DVec3::new(-0.77, -0.64, 0.0).normalize();
        let m = render_to_ecliptic(north_r, sun_r, sun_e);
        let (s, c) = OBLIQUITY.sin_cos();
        assert!((m * north_r).distance(DVec3::new(0.0, s, c)) < 1e-9);
        // The two inputs are not exactly the obliquity apart; the Sun keeps
        // its angle from the pole and lands on the ecliptic side it should.
        assert!((m * sun_r).dot(sun_e) > 0.95);
    }

    /// A view as the system builds it, for a camera looking at the Earth.
    fn view_at(alt: f64) -> View {
        let earth = DVec3::from(planets::position(&PLANETS[planets::EARTH], 1.793e9)) * AU_M;
        let mut v = View::new(alt, DVec3::X, DVec3::Z, earth, 300.0, 724.0);
        v.centre = pos2(400.0, 300.0);
        v
    }

    #[test]
    fn at_the_ceiling_the_earth_is_the_globe_s_size() {
        let v = view_at(45_000_000.0 - EARTH_R_M);
        let earth = v.middle;
        let r = v.size(earth, EARTH_R_M);
        let globe = 724.0 * EARTH_R_M / 45_000_000.0;
        assert!((f64::from(r) / globe - 1.0).abs() < 0.01, "{r} vs {globe}");
        assert_eq!(v.at(earth), pos2(400.0, 300.0));
    }

    #[test]
    fn a_climb_moves_home_smoothly_and_never_off_screen() {
        // The Earth (home) stays on screen from the ceiling to the top of the
        // rail, and a 5 % climb never moves it, or the Sun, by a jump.
        let screen = egui::Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let mut alt = 4.0e7;
        let mut last: Option<(Pos2, Pos2)> = None;
        while alt < crate::systems::zoom_rail::TOP_M {
            let v = view_at(alt);
            let earth = DVec3::from(planets::position(&PLANETS[planets::EARTH], 1.793e9)) * AU_M;
            let (e, s) = (v.at(earth), v.at(DVec3::ZERO));
            assert!(screen.contains(e), "home off screen at {alt:e}: {e:?}");
            if let Some((le, ls)) = last {
                assert!(e.distance(le) < 20.0, "Earth jumps at {alt:e}");
                // Off screen it may race in from afar; on screen it may not.
                if screen.contains(s) && screen.contains(ls) {
                    assert!(s.distance(ls) < 40.0, "Sun jumps at {alt:e}");
                }
            }
            last = Some((e, s));
            alt *= 1.05;
        }
    }

    #[test]
    fn what_is_under_the_hand_is_what_was_drawn_there() {
        // A drag reads the map back through the squeeze, at every rung.
        for alt in [1.0e8, 1.0e12, 1.0e17, 1.0e20] {
            let v = view_at(alt);
            for s in [pos2(410.0, 290.0), pos2(780.0, 20.0), pos2(30.0, 590.0)] {
                let d = v.under(s);
                let back = v.at(v.middle + v.right * d.x + v.up * d.y);
                assert!(
                    back.distance(s) < 0.01,
                    "{alt:e}: {s:?} came back as {back:?}"
                );
            }
        }
    }

    #[test]
    fn at_the_top_the_universe_fits_a_phone() {
        // The test view's half height is 300 px; a phone held upright is about
        // 0.46 of that across.
        let v = view_at(crate::systems::zoom_rail::TOP_M);
        let r = f64::from(v.ring(EDGE_M));
        assert!(r < 0.46 * 300.0 && r > 60.0, "{r}");
        // And it is drawn to scale there, not squeezed.
        assert!((r / (EDGE_M * v.px_per_m) - 1.0).abs() < 0.05, "{r}");
    }

    #[test]
    fn each_catalogue_is_on_screen_on_its_own_rung() {
        // Andromeda (2.5 million ly) on the Local Group's rung, Virgo (54
        // million) on the nearby galaxies', and the survey's reach (2 billion)
        // on the cosmic web's.
        let screen = egui::Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        for (rung, ly) in [
            (Rung::LocalGroup, 2.5e6),
            (Rung::NearbyGalaxies, 5.4e7),
            (Rung::CosmicWeb, 2.0e9),
        ] {
            let (lo, hi) = rung.altitudes();
            let v = view_at((lo * hi).sqrt());
            let q = v.at(v.middle + v.right * ly * LIGHT_YEAR_M);
            assert!(screen.contains(q), "{rung:?}: {q:?}");
        }
    }

    #[test]
    fn a_tap_on_the_milky_way_shows_all_of_it() {
        // Where the rail's "Milky Way" goes, the galactic centre is the middle
        // and every point of the galaxy is on screen.
        let v = view_at(crate::systems::zoom_rail::Scale::MilkyWay.overview());
        let gc = gal_to_ecl(DVec3::X) * GC_LY * LIGHT_YEAR_M;
        assert!(v.at(gc).distance(v.centre) < 5.0);
        let screen = egui::Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let points = galaxy_points();
        assert!(points.len() > 5_000);
        for (pt, _, _) in points {
            let g = DVec3::new(GC_LY - f64::from(pt.y), f64::from(pt.x), 0.0);
            let q = v.at(gal_to_ecl(g) * LIGHT_YEAR_M);
            assert!(screen.contains(q), "{pt:?} at {q:?}");
        }
    }

    #[test]
    fn the_sun_is_on_the_local_arm_s_inner_edge() {
        // The arms' frame and the Sun's place agree: the Local arm passes
        // just outside the Sun, within its width (Reid et al. 2019, fig. 1).
        let (_, _, kink, r_kink, psi, _, w) = ARMS[4];
        let r = r_kink * (kink.to_radians() * psi.to_radians().tan()).exp();
        let sun = GC_LY as f32 / LY_PER_KPC;
        assert!(r > sun && r - sun < 1.5 * w, "{r} kpc vs the Sun's {sun}");
    }

    #[test]
    fn the_squeeze_leaves_the_middle_to_scale() {
        assert!((squeeze(10.0, 1.0e6) - 10.0).abs() < 1e-3);
        // Each further ten-fold takes the same room.
        let c = 40.0;
        let a = squeeze(1.0e5, c) - squeeze(1.0e4, c);
        let b = squeeze(1.0e6, c) - squeeze(1.0e5, c);
        assert!((a - b).abs() < 0.5);
    }
}
