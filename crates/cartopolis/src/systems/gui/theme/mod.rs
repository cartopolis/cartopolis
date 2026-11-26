//! The Cartopolis look: one palette, one `egui::Style`, one place to change them.
//!
//! **Deep teal.** A teal-grey ramp — `#0F1416` ink, `#141A1C` surface,
//! `#1F2829` raised, `#2A3536` hover — carries the chrome, and the live things
//! are a clear **teal**: [`Palette::accent`] `#5CC6C0`, with [`Palette::ink`] on it, and its dark
//! half [`Palette::accent_muted`] `#263F42` for a selected row or a pressed button. The
//! values are read off Android's keyboard in its teal dark theme (2026-10-10),
//! which is the look asked for: the surface, the key and the shift-key teal are
//! that keyboard's own. [`Palette::warn`] and [`Palette::error`] are the only other hues, and
//! [`BRAND`] — the logo's orange — stays with the mark, the wordmark and the
//! badges, where against teal it is the logo's original pairing.
//!
//! **How it got here, so nobody walks the same circle.** The ramp was a slate
//! blue with the full brand orange as accent; then (2026-09-10) it went
//! monochrome — a blue-teal grey with a *pale grey* accent — on the argument
//! that a saturated accent competes with a colourful map; then (2026-10-04)
//! the grey was turned toward orange, the panels tinted brown, and finally a
//! neutral charcoal with an orange "ember" accent. The pale grey accent was
//! rejected because every selected tab, ticked box and engaged button read as
//! one more shade of grey — the UI looked dull — and orange mixed *into* the
//! surfaces read as brown. The charcoal and ember was replaced at the user's
//! request for a teal look shared with the web UI (`web_theme`). What survives
//! all of them is the usual rule for a dark UI: **quiet surfaces, one saturated
//! accent, used sparingly**, appearing only where something is selected,
//! engaged, pressed or focused — which keeps it meaning "this is the live
//! thing" over a map. The accent being a *saturated* teal rather than the pale
//! grey of 2026-09 is the difference that matters.
//!
//! Three consequences worth knowing before editing a value:
//!
//!   - **The state ladder is monotonic in lightness** — rest ([`Palette::surface_raised`])
//!     → hover ([`Palette::surface_hover`] plus a [`Palette::accent_word`] rim) → pressed
//!     ([`Palette::accent_muted`], a deep teal, plus the same rim) → engaged
//!     (an [`Palette::accent`] plate carrying [`Palette::on_accent`]). Each step is up, so state still
//!     reads for someone who cannot see the hue.
//!   - **Text on the accent is [`Palette::on_accent`], white** (since 2026-11-10,
//!     on a deep teal; it was dark ink on a bright one). A pressed button keeps
//!     its fill [`Palette::accent_muted`] (see [`widgets`]), so its label stays
//!     [`Palette::cream`].
//!   - **Two constants deliberately share a value.** [`Palette::accent_text`] and
//!     [`Palette::accent_bright`] are both pure white: one is type, one is a stroke, they
//!     answer to different tests. Keeping both names is what lets one move
//!     without the other.
//!
//! **Every surface is solid** (since 2026-11-08, "a bad implementation of
//! glass"): the midas hub's layering, one darkness. [`Palette::panel`], [`Palette::glass`] and
//! [`Palette::overlay`] are all one dark teal, with [`Palette::card`] blocks inside, close to it, and a soft
//! shadow, with the hub's hairline ([`Palette::panel_rim`]). Nothing shows through.
//!
//! Two things this replaces, both of which the UI used to get by default:
//!
//!   - **egui's stock dark theme**, which is a neutral grey (`gray(27)` panels)
//!     with a blue selection. Nothing about it said Cartopolis, and next to the
//!     map — which is bright — flat grey read as a hole rather than a surface.
//!   - **egui's stock spacing**, which is tuned for a debug inspector: 3 pt
//!     between rows and 1 pt of vertical button padding. In a panel of place
//!     rows and layer toggles that is a wall of text; the values here roughly
//!     double the vertical rhythm and give buttons a real hit area.
//!
//! Contrast was checked against the surface each colour actually lands on:
//! [`Palette::text`] is 14.3:1 on [`Palette::surface`], [`Palette::text_dim`] 9.4:1, [`Palette::accent_text`] 17.6:1.
//! Plain [`Palette::accent`] is 3.9:1, enough for a fill, a rule or a ring (3:1) on any
//! surface in the ramp. Anything sitting *on* an accent fill is drawn in
//! [`Palette::on_accent`], white, at 4.5:1.
//! (The numbers in this note are the dark theme's.)
//!
//! **Two palettes, since 2026-11-10.** Everything above describes [`DARK`],
//! which is the look as it was, value for value. [`LIGHT`] is the midas hub's
//! light tokens put in the same roles; the chrome reads the one in force
//! through [`p`], and `systems::ui_theme` picks it from the app's light
//! setting (Light, Dark, or Auto by the sun). The map's own labels, the splash,
//! the call and camera screens and the script editor's code box stay dark in
//! both ([`dark_here`], [`use_dark_visuals`]).
//!
//! [`install`] is called once, from `EguiManager::default`, before the first
//! pass — early enough that the brand font family is bound on frame one, which
//! is what lets the splash paint its wordmark immediately.
//!
//! **Four scales, and nothing outside them.** Colour was the only axis this file
//! originally owned, and the drift went where there was no vocabulary to drift
//! from: the app shipped 17 text sizes and 8 corner radii against one exported
//! radius, because a panel that needed "a bit smaller than body" had nothing to
//! name. So the type scale ([`SIZE_SUB`] and friends), the corner radii
//! ([`RADIUS`], [`RADIUS_SM`], [`RADIUS_LG`], [`RADIUS_PILL`]) and the touch
//! sizes ([`MIN_TOUCH_TARGET_DP`], [`row_height`]) are exported beside the
//! palette. Pick by **meaning**, never by the number — a call site that reads
//! `SIZE_SUB` tells the next person what the text *is*, and `12.5` tells them
//! only what someone once liked.

use egui::{
    style::{Selection, WidgetVisuals, Widgets},
    Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
};

use std::sync::atomic::{AtomicBool, Ordering};

use crate::systems::icons;

mod brand;
mod color;
mod controls;
mod frames;
mod header;
mod map_controls;
mod metrics;
mod palette;
mod palette32;
mod states;
mod style;
// Read by the web theme's test and by the library's backends (`ui-lib`);
// in a lane with neither it is the table nothing reads yet.
#[cfg_attr(not(any(test, feature = "ui-lib")), allow(dead_code))]
pub mod tokens;

pub use brand::*;
pub use color::Rgba8;
pub use controls::*;
pub use frames::*;
pub use header::*;
pub use map_controls::*;
pub use metrics::*;
pub use palette::*;
pub use palette32::*;
pub use states::*;
pub use style::*;
// The site's tokens (`theme.css`, written by the web theme test) and the
// app's, for the library's screens.
#[cfg(test)]
pub use tokens::tokens;
#[cfg(feature = "ui-lib")]
pub use tokens::{app_tokens, map_vars};

#[cfg(test)]
mod golden;
#[cfg(test)]
mod tests;

/// Looking at the mark, rather than reasoning about it.
///
/// `cargo test -p cartopolis write_mark_preview -- --ignored --nocapture` writes
/// `/tmp/mark_<radius>.png` at the four sizes the mark is drawn at. It
/// rasterises the **tessellated** output — the triangles egui would hand the
/// GPU — so it shows the real thing including the minimum stroke widths and the
/// detail cutoff, which is what a hand-written SVG of the same table would not.
///
/// It exists because the mark is a reduction of `assets/icon/cartopolis.svg`
/// (see [`mark`]) and a reduction can only be judged beside its source. Ignored
/// because it writes files and asserts nothing.
#[cfg(test)]
mod mark_preview {
    use super::*;
    use crate::systems::egui_manager::UnpaintedPasses as _;

    /// Sizes: the splash, a mid size, the sign-in card, and one below
    /// [`mark::DETAIL_RADIUS`] to see what the cutoff leaves.
    const PREVIEW_RADII: [f32; 4] = [84.0, 40.0, 22.0, 12.0];

    #[test]
    #[ignore = "eyeball hatch: it writes PNGs to look at, it asserts nothing"]
    fn write_mark_preview() {
        let ppp = 2.0_f32;
        for radius in PREVIEW_RADII {
            let pad = 8.0;
            let side = radius * 2.0 + pad * 2.0;
            let ctx = egui::Context::default();
            install(&ctx);
            ctx.set_pixels_per_point(ppp);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(side, side));
            let out = ctx.run_ui_unpainted(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ui| {
                    let p = ui.painter();
                    p.rect_filled(screen, 0.0, DARK.surface);
                    paint_mark(p, screen.center(), radius, 1.0);
                },
            );
            let prims = ctx.tessellate(out.shapes, ppp);
            let w = (side * ppp) as usize;
            let mut buf = vec![[0.0_f32; 4]; w * w];
            for prim in prims {
                let egui::epaint::Primitive::Mesh(mesh) = prim.primitive else {
                    continue;
                };
                for tri in mesh.indices.chunks(3) {
                    let v: Vec<_> = tri.iter().map(|&i| mesh.vertices[i as usize]).collect();
                    let (p0, p1, p2) = (v[0].pos * ppp, v[1].pos * ppp, v[2].pos * ppp);
                    let area = (p1.x - p0.x) * (p2.y - p0.y) - (p2.x - p0.x) * (p1.y - p0.y);
                    if area.abs() < 1e-6 {
                        continue;
                    }
                    let minx = p0.x.min(p1.x).min(p2.x).floor().max(0.0) as usize;
                    let maxx = (p0.x.max(p1.x).max(p2.x).ceil() as usize).min(w - 1);
                    let miny = p0.y.min(p1.y).min(p2.y).floor().max(0.0) as usize;
                    let maxy = (p0.y.max(p1.y).max(p2.y).ceil() as usize).min(w - 1);
                    for y in miny..=maxy {
                        for x in minx..=maxx {
                            let px = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                            let w0 = ((p1.x - px.x) * (p2.y - px.y)
                                - (p2.x - px.x) * (p1.y - px.y))
                                / area;
                            let w1 = ((p2.x - px.x) * (p0.y - px.y)
                                - (p0.x - px.x) * (p2.y - px.y))
                                / area;
                            let w2 = 1.0 - w0 - w1;
                            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                                continue;
                            }
                            let mut src = [0.0_f32; 4];
                            for (k, vert) in v.iter().enumerate() {
                                let bw = [w0, w1, w2][k];
                                let c = vert.color;
                                // Vertex colours are premultiplied sRGB bytes.
                                src[0] += c.r() as f32 * bw;
                                src[1] += c.g() as f32 * bw;
                                src[2] += c.b() as f32 * bw;
                                src[3] += c.a() as f32 * bw;
                            }
                            let dst = &mut buf[y * w + x];
                            let inv = 1.0 - src[3] / 255.0;
                            for k in 0..4 {
                                dst[k] = src[k] + dst[k] * inv;
                            }
                        }
                    }
                }
            }
            // Downsample by `ppp` so the file is the size the mark is drawn at.
            let out_w = w / ppp as usize;
            let mut img = image::RgbaImage::new(out_w as u32, out_w as u32);
            let n = ppp as usize;
            for y in 0..out_w {
                for x in 0..out_w {
                    let mut acc = [0.0_f32; 4];
                    for dy in 0..n {
                        for dx in 0..n {
                            let s = buf[(y * n + dy) * w + x * n + dx];
                            for k in 0..4 {
                                acc[k] += s[k];
                            }
                        }
                    }
                    let d = (n * n) as f32;
                    img.put_pixel(
                        x as u32,
                        y as u32,
                        image::Rgba([
                            (acc[0] / d) as u8,
                            (acc[1] / d) as u8,
                            (acc[2] / d) as u8,
                            255,
                        ]),
                    );
                }
            }
            let path = format!("/tmp/mark_{}.png", radius as i32);
            img.save(&path).unwrap();
            println!("wrote {path} ({out_w}px)");
        }
    }
}
