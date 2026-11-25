//! The launch splash drawn by the component library (`lib_host`), beside
//! egui's (`splash::boot_splash`): Groningen's street plan on the page
//! colour, the mark, the wordmark and motto, the progress track and the line
//! under it, until the first tiles are up — then the whole layer fades off
//! the world.
//!
//! On with `CARTO_UI_LIB=splash` (or `1`), on the web `?ui=lib`. egui's
//! system steps aside (`BootSplash::lib`) but keeps the clock, the bar's
//! latch, the caption and the dismissal, and publishes them in
//! `BootSplash::lib_view`; this module only draws. Nothing on it is
//! tapped, and it takes no input (egui's takes none either).
//!
//! **The street plan and the mark are pictures.** On the Bevy backend both
//! are rasterised once, at the first frame that needs them, with
//! `tiny-skia` into a Bevy `Image` at the window's physical pixels, and shown
//! through `mithril_bevy::LiveImages` (`<img src="bevy:splash_streets">`):
//! the streets as the same polylines egui strokes, at the same widths, in
//! the same ink ([`splash::streets_ink`]); the mark from
//! `assets/icon/cartopolis.svg`, the file egui's `theme::paint_mark` is a
//! reduction of (the tile keeps its gradient here). On the web's DOM backend
//! the same SVG texts go in as `data:image/svg+xml` sources. The Bevy
//! backend does not fade an `<img>` with its box's `opacity`, so
//! [`fade_lib_splash`] sets the two pictures' tint after the library has
//! styled them; both are dropped when the splash goes.
//!
//! **Placement.** As egui's: everything on the window's centre — the mark's
//! middle 104 above it, the wordmark 24 below, the motto 66, the track 104
//! and the caption 128 — and the plan covering the window
//! ([`splash::streets_cover`]). Over every other library screen
//! (`z-index`), as egui's is the top `Foreground` layer.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{warn, Res, ResMut};
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
use bevy::prelude::{Assets, Color, Image, ImageNode, Query};
use mithril::prelude::{el, signal, IntoView, Signal, View};

use crate::systems::egui_manager::EguiRef;
use crate::systems::lib_settings::LibScreens;
use crate::systems::splash::{self, BootSplash, SplashView};
use crate::systems::theme;
use crate::systems::theme::tokens::{alpha, hex};

/// The splash's name in the library's host.
const KEY: &str = "splash";

/// The street plan's name in `mithril_bevy::LiveImages`.
#[cfg_attr(all(feature = "ui-dom", target_arch = "wasm32"), allow(dead_code))]
const STREETS: &str = "splash_streets";

/// The mark's name in `mithril_bevy::LiveImages`.
#[cfg_attr(all(feature = "ui-dom", target_arch = "wasm32"), allow(dead_code))]
const MARK: &str = "splash_mark";

/// The app mark as an SVG: the file the launcher icon and `index.html`'s
/// loader are made from (`tools/brand-mark.ts`).
const ICON_SVG: &str = include_str!("../../../../../assets/icon/cartopolis.svg");

/// Half the mark's square, egui's figure: 84 pt of radius.
const MARK_R: f32 = 84.0;

/// Over every other library screen.
const Z: i32 = 1000;

#[derive(Clone)]
struct Live {
    view: Signal<SplashView>,
    label: Signal<String>,
    /// The library's px per egui point: 1 on the Bevy backend, the UI scale
    /// as DOM (`lib_place_card::Placement::over_the_dock` says why).
    k: f32,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the splash. `PostUpdate`, before `DriveUi`,
/// as every library screen.
pub fn drive_lib_splash(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut splash: ResMut<BootSplash>,
    egui: EguiRef,
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    mut images: ResMut<Assets<Image>>,
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    mut live_images: Option<ResMut<mithril_bevy::LiveImages>>,
) {
    if !lib.splash {
        return;
    }
    if !splash.lib {
        splash.lib = true;
    }
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        egui.ui_scale()
    } else {
        1.0
    };
    let view = splash.lib_view.clone();
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // Gone, or a new UI scale to lay out in.
        let stale = m.as_ref().is_some_and(|live| view.is_none() || live.k != k);
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let Some(view) = view else {
            // The pictures go with the splash: nothing shows them again.
            #[cfg(all(
                feature = "ui-probe",
                not(all(feature = "ui-dom", target_arch = "wasm32"))
            ))]
            if let Some(live_images) = live_images.as_mut() {
                if live_images.0.contains_key(STREETS) || live_images.0.contains_key(MARK) {
                    live_images.0.remove(STREETS);
                    live_images.0.remove(MARK);
                }
            }
            return;
        };
        match m.as_mut() {
            None => {
                // The pictures before the view that shows them: an `<img>`
                // reads its source when it is styled.
                #[cfg(all(
                    feature = "ui-probe",
                    not(all(feature = "ui-dom", target_arch = "wasm32"))
                ))]
                if let Some(live_images) = live_images.as_mut() {
                    if !live_images.0.contains_key(STREETS) {
                        if let Some(image) = raster::streets(view.size, view.physical) {
                            live_images.0.insert(STREETS.to_owned(), images.add(image));
                        }
                    }
                    if !live_images.0.contains_key(MARK) {
                        let ppp = view.physical[0] as f32 / view.size.x.max(1.0);
                        let side = (2.0 * MARK_R * ppp).round() as u32;
                        if let Some(image) = raster::mark(side) {
                            live_images.0.insert(MARK.to_owned(), images.add(image));
                        }
                    }
                }
                let live = Live {
                    label: signal(view.label.clone()),
                    view: signal(view),
                    k,
                };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    view_of(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(&live)) {
                        warn!(?path, error = %e, "lib splash: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                if live.label.peek(|now| *now != view.label) {
                    live.label.set(view.label.clone());
                }
                if live.view.peek(|now| *now != view) {
                    live.view.set(view);
                }
            }
        }
    });
}

/// The Bevy backend draws an `<img>` untinted whatever its box's `opacity`,
/// so the two fades reach the pictures here: the plan with the backdrop,
/// the mark with the marks. `PostUpdate`, **after** `DriveUi`, which
/// resets the tint whenever it styles the node again.
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
pub fn fade_lib_splash(
    splash: Res<BootSplash>,
    live_images: Option<Res<mithril_bevy::LiveImages>>,
    mut nodes: Query<&mut ImageNode>,
) {
    let (Some(view), Some(live_images)) = (splash.lib_view.as_ref(), live_images) else {
        return;
    };
    let streets = live_images.0.get(STREETS);
    let mark = live_images.0.get(MARK);
    if streets.is_none() && mark.is_none() {
        return;
    }
    for mut node in &mut nodes {
        let a = if streets == Some(&node.image) {
            view.backdrop
        } else if mark == Some(&node.image) {
            view.content
        } else {
            continue;
        };
        let want = Color::srgba(1.0, 1.0, 1.0, a.clamp(0.0, 1.0));
        if node.color != want {
            node.color = want;
        }
    }
}

/// The HTML from the library's string renderer, for the browser beside a
/// capture.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view_of(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

/// `v` points as the library's px.
fn px(v: f32, k: f32) -> String {
    format!("{:.1}px", v * k)
}

/// A band the window's width, `h` points tall, centred `dy` below the
/// window's middle, its content centred in it.
fn band(v: &SplashView, dy: f32, h: f32, k: f32) -> String {
    format!(
        "position: absolute; left: 0; top: {}; width: {}; height: {}; \
         display: flex; align-items: center; justify-content: center; white-space: nowrap",
        px(v.size.y * 0.5 + dy - h * 0.5, k),
        px(v.size.x, k),
        px(h, k)
    )
}

/// The street plan's source: the Bevy backend's raster, or the web's SVG in
/// the streets' ink.
fn streets_src() -> String {
    #[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
    {
        let ink = format!("\"{}\"", hex(splash::streets_ink()));
        let svg = splash::STREETS_SVG.replace("\"#000\"", &ink);
        format!("data:image/svg+xml;base64,{}", base64(svg.as_bytes()))
    }
    #[cfg(not(all(feature = "ui-dom", target_arch = "wasm32")))]
    {
        format!("bevy:{STREETS}")
    }
}

/// The mark's source, as [`streets_src`].
fn mark_src() -> String {
    #[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
    {
        format!("data:image/svg+xml;base64,{}", base64(ICON_SVG.as_bytes()))
    }
    #[cfg(not(all(feature = "ui-dom", target_arch = "wasm32")))]
    {
        let _ = ICON_SVG;
        format!("bevy:{MARK}")
    }
}

/// Standard base64 with padding. The `base64` crate is not in the web
/// build.
#[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ABC[(n >> 18) as usize & 63] as char);
        out.push(ABC[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ABC[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ABC[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The root: the whole window, listening nowhere; the backdrop and the plan
/// under the marks, which fade in on their own.
fn view_of(l: &Live) -> View {
    let k = l.k;
    let root = format!(
        "position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: {Z}; \
         pointer-events: none; overflow: hidden"
    );
    // The page colour, opaque from the first frame: what is behind it is a
    // world with nothing streamed in yet (`splash::FADE_IN_S`).
    let backdrop = {
        let v = l.view.clone();
        move || -> String {
            let v = v.get();
            format!(
                "position: absolute; left: 0; top: 0; width: {}; height: {}; \
                 background: {}; opacity: {:.3}",
                px(v.size.x, k),
                px(v.size.y, k),
                hex(theme::DARK.surface),
                v.backdrop
            )
        }
    };
    let streets = {
        let v = l.view.clone();
        move || -> String {
            let v = v.get();
            // The Bevy backend's raster is the window already cropped; the
            // web's SVG is placed to cover it.
            let (left, top, w, h) = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
                let (plan, _) = splash::streets();
                let (origin, scale) = splash::streets_cover(v.size);
                (origin.x, origin.y, plan.x * scale, plan.y * scale)
            } else {
                (0.0, 0.0, v.size.x, v.size.y)
            };
            format!(
                "position: absolute; left: {}; top: {}; width: {}; height: {}; opacity: {:.3}",
                px(left, k),
                px(top, k),
                px(w, k),
                px(h, k),
                v.backdrop
            )
        }
    };
    let content = {
        let v = l.view.clone();
        move || -> String {
            let v = v.get();
            format!(
                "position: absolute; left: 0; top: 0; width: {}; height: {}; opacity: {:.3}",
                px(v.size.x, k),
                px(v.size.y, k),
                v.content
            )
        }
    };
    el("div")
        .attr("style", root)
        .child(el("div").attr("style", backdrop))
        .child(
            el("img")
                .attr("src", streets_src())
                .attr("alt", "")
                .attr("style", streets),
        )
        .child(
            el("div")
                .attr("style", content)
                .child(mark_view(l))
                .child(wordmark_view(l))
                .child(motto_view(l))
                .child(track_view(l))
                .child(caption_view(l)),
        )
        .into_view()
}

/// The mark, 168 pt square, its middle 104 above the window's.
fn mark_view(l: &Live) -> View {
    let k = l.k;
    let style = {
        let v = l.view.clone();
        move || -> String {
            let v = v.get();
            format!(
                "position: absolute; left: {}; top: {}; width: {}; height: {}",
                px(v.size.x * 0.5 - MARK_R, k),
                px(v.size.y * 0.5 - 104.0 - MARK_R, k),
                px(2.0 * MARK_R, k),
                px(2.0 * MARK_R, k)
            )
        }
    };
    el("img")
        .attr("src", mark_src())
        .attr("alt", "Cartopolis")
        .attr("style", style)
        .into_view()
}

/// Cream `carto`, orange `polis`, in the brand face (`theme::paint_wordmark`).
fn wordmark_view(l: &Live) -> View {
    let k = l.k;
    let size = theme::SPLASH_WORDMARK;
    let style = {
        let v = l.view.clone();
        move || -> String {
            format!(
                "{}; font-size: {}; font-weight: 700; line-height: 1",
                band(&v.get(), 24.0, size * 1.5, k),
                px(size, k)
            )
        }
    };
    let head = format!("color: {}", hex(theme::DARK.cream));
    let tail = format!("color: {}", hex(theme::WORDMARK_TAIL_COLOR));
    el("div")
        .attr("style", style)
        .child(el("span").attr("style", head).child(theme::WORDMARK_HEAD))
        .child(el("span").attr("style", tail).child(theme::WORDMARK_TAIL))
        .into_view()
}

/// The motto under the wordmark.
fn motto_view(l: &Live) -> View {
    let k = l.k;
    let size = theme::SPLASH_MOTTO;
    let style = {
        let v = l.view.clone();
        move || -> String {
            format!(
                "{}; font-size: {}; line-height: 1.2; color: {}",
                band(&v.get(), 66.0, size * 1.6, k),
                px(size, k),
                alpha(theme::DARK.text_dim, 90.0)
            )
        }
    };
    el("div")
        .attr("style", style)
        .child(splash::MOTTO)
        .into_view()
}

/// The track, `min(22rem, 70vw)` wide, and its lit part.
fn track_view(l: &Live) -> View {
    let k = l.k;
    let h = splash::TRACK_H;
    let track = {
        let v = l.view.clone();
        move || -> String {
            let v = v.get();
            let w = splash::track_width(v.size.x);
            format!(
                "position: absolute; left: {}; top: {}; width: {}; height: {}; \
                 border-radius: {}; background: {}",
                px(v.size.x * 0.5 - w * 0.5, k),
                px(v.size.y * 0.5 + 104.0 - h * 0.5, k),
                px(w, k),
                px(h, k),
                px(h * 0.5, k),
                hex(theme::DARK.surface_raised)
            )
        }
    };
    let bar = {
        let v = l.view.clone();
        move || -> String {
            let v = v.get();
            let w = splash::track_width(v.size.x);
            let (left, width) = v.bar;
            format!(
                "position: absolute; left: {}; top: 0; width: {}; height: {}; \
                 border-radius: {}; background: {}",
                px(left * w, k),
                px(width * w, k),
                px(h, k),
                px(h * 0.5, k),
                hex(theme::DARK.accent)
            )
        }
    };
    el("div")
        .attr("style", track)
        .child(el("div").attr("style", bar))
        .into_view()
}

/// What the splash is waiting for, under the track.
fn caption_view(l: &Live) -> View {
    let k = l.k;
    let size = splash::STATUS_TEXT;
    let style = {
        let v = l.view.clone();
        move || -> String {
            format!(
                "{}; font-size: {}; line-height: 1.2; color: {}",
                band(&v.get(), 128.0, size * 1.6, k),
                px(size, k),
                alpha(theme::DARK.text_dim, 75.0)
            )
        }
    };
    let label = l.label.clone();
    el("div")
        .attr("style", style)
        .child(move || label.get())
        .into_view()
}

/// The two pictures, drawn once with `tiny-skia` for the Bevy backend.
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
mod raster {
    use bevy::asset::RenderAssetUsages;
    use bevy::prelude::Image;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
    use tiny_skia::{
        Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, Path, PathBuilder,
        Pixmap, Point, Rect, Shader, SpreadMode, Stroke, Transform,
    };

    use super::ICON_SVG;
    use crate::systems::splash;

    /// The most pixels a side may have: a 4K window and then some.
    const MAX_SIDE: u32 = 4096;

    /// The street plan covering a `size`-point window drawn at `physical`
    /// pixels: egui's polylines, widths and ink (`splash::paint_streets`),
    /// with round ends and joins.
    pub fn streets(size: egui::Vec2, physical: [u32; 2]) -> Option<Image> {
        let (w, h) = (
            physical[0].clamp(1, MAX_SIDE),
            physical[1].clamp(1, MAX_SIDE),
        );
        let mut pixmap = Pixmap::new(w, h)?;
        let ppp = w as f32 / size.x.max(1.0);
        let (_, layers) = splash::streets();
        let (origin, scale) = splash::streets_cover(size);
        let ink = splash::streets_ink();
        let mut paint = Paint::default();
        paint.set_color_rgba8(ink.r(), ink.g(), ink.b(), 255);
        paint.anti_alias = true;
        for layer in layers {
            let mut pb = PathBuilder::new();
            for line in &layer.lines {
                let mut points = line.iter().map(|p| {
                    (
                        (origin.x + p.x * scale) * ppp,
                        (origin.y + p.y * scale) * ppp,
                    )
                });
                if let Some((x, y)) = points.next() {
                    pb.move_to(x, y);
                    for (x, y) in points {
                        pb.line_to(x, y);
                    }
                }
            }
            let Some(path) = pb.finish() else { continue };
            let stroke = Stroke {
                width: (layer.width * scale).max(0.6) * ppp,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
        Some(image_of(&pixmap, Some([ink.r(), ink.g(), ink.b()])))
    }

    /// The mark, `side` pixels square, from the icon's SVG: its rounded
    /// tile with the gradient, the plan's strokes, the pin and its dot —
    /// every element the generator writes (`rect`, `path` of `M`/`L`/`Z`,
    /// `circle`, one `linearGradient`).
    pub fn mark(side: u32) -> Option<Image> {
        let side = side.clamp(16, MAX_SIDE);
        let mut pixmap = Pixmap::new(side, side)?;
        let unit = view_box_width(ICON_SVG).unwrap_or(512.0);
        let t = Transform::from_scale(side as f32 / unit, side as f32 / unit);
        let stops: Vec<GradientStop> = elements(ICON_SVG)
            .filter(|(name, _)| *name == "stop")
            .filter_map(|(_, tag)| {
                let at: f32 = attr(tag, "offset")?.parse().ok()?;
                Some(GradientStop::new(at, colour(&attr(tag, "stop-color")?)?))
            })
            .collect();
        for (name, tag) in elements(ICON_SVG) {
            match name {
                "rect" => {
                    let (Some(w), Some(h)) = (num(tag, "width"), num(tag, "height")) else {
                        continue;
                    };
                    let (x, y) = (num(tag, "x").unwrap_or(0.0), num(tag, "y").unwrap_or(0.0));
                    let Some(path) = rounded_rect(x, y, w, h, num(tag, "rx").unwrap_or(0.0)) else {
                        continue;
                    };
                    let shader = match attr(tag, "fill") {
                        // The one gradient: top to bottom over the box.
                        Some(f) if f.starts_with("url(") => LinearGradient::new(
                            Point::from_xy(x, y),
                            Point::from_xy(x, y + h),
                            stops.clone(),
                            SpreadMode::Pad,
                            Transform::identity(),
                        ),
                        Some(f) => colour(&f).map(Shader::SolidColor),
                        None => None,
                    };
                    let Some(shader) = shader else { continue };
                    let paint = Paint {
                        shader,
                        anti_alias: true,
                        ..Paint::default()
                    };
                    pixmap.fill_path(&path, &paint, FillRule::Winding, t, None);
                }
                "path" => {
                    let Some(path) = attr(tag, "d").and_then(|d| path_of(&d)) else {
                        continue;
                    };
                    if let Some(c) = attr(tag, "fill")
                        .filter(|f| f != "none")
                        .and_then(|f| colour(&f))
                    {
                        pixmap.fill_path(&path, &solid(c), FillRule::Winding, t, None);
                    }
                    if let Some(c) = attr(tag, "stroke").and_then(|s| colour(&s)) {
                        let round = |a: &str| attr(tag, a).is_some_and(|v| v == "round");
                        let stroke = Stroke {
                            width: num(tag, "stroke-width").unwrap_or(1.0),
                            line_cap: if round("stroke-linecap") {
                                LineCap::Round
                            } else {
                                LineCap::Butt
                            },
                            line_join: if round("stroke-linejoin") {
                                LineJoin::Round
                            } else {
                                LineJoin::Miter
                            },
                            ..Stroke::default()
                        };
                        pixmap.stroke_path(&path, &solid(c), &stroke, t, None);
                    }
                }
                "circle" => {
                    let (Some(cx), Some(cy), Some(r)) =
                        (num(tag, "cx"), num(tag, "cy"), num(tag, "r"))
                    else {
                        continue;
                    };
                    let Some(c) = attr(tag, "fill").and_then(|f| colour(&f)) else {
                        continue;
                    };
                    if let Some(path) = PathBuilder::from_circle(cx, cy, r) {
                        pixmap.fill_path(&path, &solid(c), FillRule::Winding, t, None);
                    }
                }
                _ => {}
            }
        }
        Some(image_of(&pixmap, None))
    }

    fn solid(c: Color) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color(c);
        paint.anti_alias = true;
        paint
    }

    /// Each element's name and its attributes' text, in document order.
    fn elements(svg: &str) -> impl Iterator<Item = (&str, &str)> {
        svg.split('<').skip(1).map(|tag| {
            let tag = tag.split('>').next().unwrap_or(tag);
            let name = tag
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("");
            (name, tag)
        })
    }

    /// An attribute's value, as `splash::parse_streets` reads them.
    fn attr(tag: &str, name: &str) -> Option<String> {
        let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
        Some(tag[at..].split('"').next()?.to_owned())
    }

    fn num(tag: &str, name: &str) -> Option<f32> {
        attr(tag, name)?.trim().parse().ok()
    }

    fn view_box_width(svg: &str) -> Option<f32> {
        let (_, tag) = elements(svg).find(|(name, _)| *name == "svg")?;
        attr(tag, "viewBox")?
            .split_whitespace()
            .nth(2)?
            .parse()
            .ok()
    }

    /// `#rrggbb`, opaque.
    fn colour(s: &str) -> Option<Color> {
        let h = s.trim().strip_prefix('#')?;
        if h.len() != 6 {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        Some(Color::from_rgba8(byte(0)?, byte(2)?, byte(4)?, 255))
    }

    fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
        let r = r.min(w * 0.5).min(h * 0.5);
        if r <= 0.0 {
            return Some(PathBuilder::from_rect(Rect::from_xywh(x, y, w, h)?));
        }
        // A quarter circle's Bézier handle.
        let c = 0.552_284_8 * r;
        let mut pb = PathBuilder::new();
        pb.move_to(x + r, y);
        pb.line_to(x + w - r, y);
        pb.cubic_to(x + w - r + c, y, x + w, y + r - c, x + w, y + r);
        pb.line_to(x + w, y + h - r);
        pb.cubic_to(x + w, y + h - r + c, x + w - r + c, y + h, x + w - r, y + h);
        pb.line_to(x + r, y + h);
        pb.cubic_to(x + r - c, y + h, x, y + h - r + c, x, y + h - r);
        pb.line_to(x, y + r);
        pb.cubic_to(x, y + r - c, x + r - c, y, x + r, y);
        pb.close();
        pb.finish()
    }

    /// An SVG path of absolute `M`, `L` and `Z`: all the icon's generator
    /// writes.
    fn path_of(d: &str) -> Option<Path> {
        struct Pen {
            pb: PathBuilder,
            cmd: char,
            started: bool,
            numbers: Vec<f32>,
            token: String,
        }
        impl Pen {
            fn flush(&mut self) {
                if let Ok(n) = self.token.parse::<f32>() {
                    self.numbers.push(n);
                }
                self.token.clear();
                if self.numbers.len() == 2 {
                    let (x, y) = (self.numbers[0], self.numbers[1]);
                    self.numbers.clear();
                    if self.cmd == 'M' && !self.started {
                        self.pb.move_to(x, y);
                        self.started = true;
                    } else {
                        self.pb.line_to(x, y);
                    }
                }
            }
        }
        let mut pen = Pen {
            pb: PathBuilder::new(),
            cmd: 'M',
            started: false,
            numbers: Vec::new(),
            token: String::new(),
        };
        for ch in d.chars() {
            match ch {
                c if c.is_ascii_alphabetic() => {
                    pen.flush();
                    pen.numbers.clear();
                    match c {
                        'Z' | 'z' => pen.pb.close(),
                        'M' => pen.started = false,
                        _ => {}
                    }
                    pen.cmd = c.to_ascii_uppercase();
                }
                '-' => {
                    pen.flush();
                    pen.token.push('-');
                }
                c if c.is_ascii_digit() || c == '.' => pen.token.push(c),
                _ => pen.flush(),
            }
        }
        pen.flush();
        pen.pb.finish()
    }

    /// The pixmap as a Bevy image, straight alpha. `bleed` gives the clear
    /// pixels that colour rather than black, so a filtered edge does not
    /// darken.
    fn image_of(pixmap: &Pixmap, bleed: Option<[u8; 3]>) -> Image {
        let mut data = Vec::with_capacity(pixmap.data().len());
        for p in pixmap.pixels() {
            let c = p.demultiply();
            match bleed {
                Some([r, g, b]) if c.alpha() == 0 => data.extend_from_slice(&[r, g, b, 0]),
                _ => data.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]),
            }
        }
        Image::new(
            Extent3d {
                width: pixmap.width(),
                height: pixmap.height(),
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            data,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_path_moves_draws_and_closes() {
            let path = path_of("M0 0L10 0L10 -5ZM3 3L4 4").expect("a path");
            let b = path.bounds();
            assert_eq!(
                (b.left(), b.top(), b.right(), b.bottom()),
                (0.0, -5.0, 10.0, 4.0)
            );
        }

        /// The mark draws: the tile is opaque in the middle and clear in a
        /// rounded corner.
        #[test]
        fn the_mark_rasterises() {
            let image = mark(64).expect("an image");
            let data = image.data.as_ref().expect("pixels");
            let at = |x: usize, y: usize| data[(y * 64 + x) * 4 + 3];
            assert_eq!(at(0, 0), 0, "the corner is rounded off");
            assert_eq!(at(32, 2), 255, "the tile is opaque");
        }
    }
}
