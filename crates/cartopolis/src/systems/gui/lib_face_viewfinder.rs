//! The polaroid's viewfinder drawn by the component library (`lib_host`),
//! beside egui's (`faces::render_face_viewfinder`): a square over the live
//! map, the world outside it dimmed, and Cancel, Face me and Take it under
//! it.
//!
//! On with `CARTO_UI_LIB=face_viewfinder` (or `1`), on the web `?ui=lib`.
//! egui's system steps aside (`Faces::lib`) but still owns the viewfinder —
//! when it is up, the camera Face me swings, the capture the shutter asks
//! for — and publishes `Faces::lib_view`. A tap here goes back in
//! `Faces::lib_acts` and is done by `Faces::finder_act`, the code egui's
//! buttons call.
//!
//! **The square is a hole.** Nothing is drawn in it: the shade is four
//! boxes around it and the rim a bordered box with no fill, so the map
//! shows through and, as with egui's painted shade, every part of it but
//! the buttons lets the pointer through to the map — framing the picture is
//! dragging the world. **Placement.** As egui's: the square
//! `faces::finder_rect` of the window, its 2 px cream rim outside it; the
//! buttons in a row centred `FINDER_CONTROLS_BOTTOM` above the bottom
//! edge, wrapping at a phone's width rather than running off both sides as
//! egui's row does.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{warn, Res, ResMut};
use mithril::prelude::{el, signal, Event, IntoView, Signal, View};

use crate::systems::faces::{finder_rect, Faces, FinderAct, FinderView, FINDER_CONTROLS_BOTTOM};
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme::tokens::{alpha, hex};
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The viewfinder's name in the library's host.
const KEY: &str = "face_viewfinder";

/// Over the other library screens, as egui's is a `Foreground` layer; under
/// the launch splash (`lib_splash`).
const Z: i32 = 900;

/// egui's shade: black at 110 of 255.
const SHADE_PCT: f32 = 110.0 / 255.0 * 100.0;

/// The rim round the square, px, outside it (`StrokeKind::Outside`).
const RIM: f32 = 2.0;

/// The window the viewfinder is placed in.
#[derive(Clone, Copy, PartialEq, Default)]
struct Placement {
    /// The viewport, egui points.
    w: f32,
    h: f32,
    /// The library's px per egui point: 1 on the Bevy backend, the UI scale
    /// as DOM (`lib_place_card::Placement::over_the_dock` says why).
    k: f32,
}

impl Placement {
    fn of(size: egui::Vec2, ui_scale: f32) -> Self {
        let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            ui_scale
        } else {
            1.0
        };
        Placement {
            w: size.x,
            h: size.y,
            k,
        }
    }

    /// `v` points as the library's px.
    fn px(&self, v: f32) -> String {
        format!("{:.1}px", v * self.k)
    }

    /// A box at `r`, egui points.
    fn at(&self, r: egui::Rect) -> String {
        format!(
            "position: absolute; left: {}; top: {}; width: {}; height: {}",
            self.px(r.min.x),
            self.px(r.min.y),
            self.px(r.width().max(0.0)),
            self.px(r.height().max(0.0))
        )
    }

    fn screen(&self) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(self.w, self.h))
    }
}

type Acts = Rc<RefCell<Vec<FinderAct>>>;

#[derive(Clone)]
struct Live {
    view: Signal<FinderView>,
    placement: Signal<Placement>,
    acts: Acts,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// A tap that hands `a` back.
fn on(acts: &Acts, a: FinderAct) -> impl Fn(&Event) + 'static {
    let acts = acts.clone();
    move |_: &Event| acts.borrow_mut().push(a)
}

/// Mounts, bridges and unmounts the viewfinder. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_face_viewfinder(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut faces: ResMut<Faces>,
    metrics: bevy::prelude::Res<UiMetrics>,
) {
    if !lib.face_viewfinder {
        return;
    }
    if !faces.lib {
        faces.lib = true;
    }
    let viewport = metrics.viewport_pts();
    let placement = Placement::of(egui::vec2(viewport.x, viewport.y), metrics.ui_scale());
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, for egui's system to do next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<FinderAct> = std::mem::take(&mut *live.acts.borrow_mut());
            if !acts.is_empty() {
                faces.lib_acts.extend(acts);
            }
        }
        let Some(view) = faces.lib_view.clone() else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        };
        match m.as_mut() {
            None => {
                let live = Live {
                    view: signal(view),
                    placement: signal(placement),
                    acts: Rc::new(RefCell::new(Vec::new())),
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
                        warn!(?path, error = %e, "lib face viewfinder: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                if live.view.peek(|now| *now != view) {
                    live.view.set(view);
                }
                if live.placement.get_untracked() != placement {
                    live.placement.set(placement);
                }
            }
        }
    });
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

/// The root: the whole window, listening nowhere — the shade, the rim and
/// the buttons placed in it.
fn view_of(l: &Live) -> View {
    let root = format!(
        "position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: {Z}; \
         pointer-events: none"
    );
    let shade = alpha(theme::Rgba8::BLACK, SHADE_PCT);
    // Four boxes round the square: above, below, left and right of it.
    let sides: Vec<View> = (0..4)
        .map(|i| {
            let placement = l.placement.clone();
            let shade = shade.clone();
            let style = move || -> String {
                let p = placement.get();
                let (s, f) = (p.screen(), finder_rect(p.screen()));
                let r = match i {
                    0 => egui::Rect::from_min_max(s.min, egui::pos2(s.max.x, f.min.y)),
                    1 => egui::Rect::from_min_max(egui::pos2(s.min.x, f.max.y), s.max),
                    2 => egui::Rect::from_min_max(
                        egui::pos2(s.min.x, f.min.y),
                        egui::pos2(f.min.x, f.max.y),
                    ),
                    _ => egui::Rect::from_min_max(
                        egui::pos2(f.max.x, f.min.y),
                        egui::pos2(s.max.x, f.max.y),
                    ),
                };
                format!("{}; background: {shade}; pointer-events: none", p.at(r))
            };
            el("div").attr("style", style).into_view()
        })
        .collect();
    // The rim: a bordered box with no fill, just outside the square.
    let cream = hex(theme::p().cream);
    let rim = {
        let placement = l.placement.clone();
        move || -> String {
            let p = placement.get();
            let r = finder_rect(p.screen()).expand(RIM);
            format!(
                "{}; box-sizing: border-box; border: {} solid {cream}; \
                 background: transparent; pointer-events: none",
                p.at(r),
                p.px(RIM)
            )
        }
    };
    let controls = {
        let (view, placement, acts) = (l.view.clone(), l.placement.clone(), l.acts.clone());
        move || -> View { controls_view(&view.get(), &placement.get(), &acts) }
    };
    el("div")
        .attr("style", root)
        .children(sides)
        .child(el("div").attr("style", rim))
        .child(controls)
        .into_view()
}

/// A glyph of the icon face at `size` px.
fn glyph(g: &'static str, size: f32, p: &Placement) -> View {
    el("span")
        .attr(
            "style",
            format!(
                "font-family: 'Material Symbols Rounded'; font-size: {}; line-height: 1",
                p.px(size)
            ),
        )
        .child(g)
        .into_view()
}

/// Cancel, Face me and the shutter, in a row centred over the bottom edge.
fn controls_view(v: &FinderView, p: &Placement, acts: &Acts) -> View {
    let band = format!(
        "position: absolute; left: 0; bottom: {}; width: {}; box-sizing: border-box; \
         padding: 0 {}; display: flex; flex-wrap: wrap; align-items: center; \
         justify-content: center; gap: {}; pointer-events: none",
        p.px(FINDER_CONTROLS_BOTTOM),
        p.px(p.w),
        p.px(theme::OSD_MARGIN),
        p.px(8.0)
    );
    // egui's `pill`: the raised surface, rounded 12, 32 tall, 12.5 text.
    let pill = format!(
        "display: flex; align-items: center; gap: {}; min-height: {}; box-sizing: border-box; \
         padding: 0 {}; border-radius: {}; background: var(--m-surface-raised); \
         color: var(--m-text); font-size: {}; white-space: nowrap; cursor: pointer; \
         pointer-events: auto",
        p.px(6.0),
        p.px(32.0),
        p.px(12.0),
        p.px(12.0),
        p.px(theme::SIZE_SUB)
    );
    let button = |icon: &'static str, words: &'static str, act: FinderAct| -> View {
        el("div")
            .attr("style", pill.clone())
            .attr("role", "button")
            .attr("aria-label", words)
            .on("click", on(acts, act))
            .child(glyph(icon, 16.0, p))
            .child(el("span").child(words))
            .into_view()
    };
    // The shutter: the accent, rounded 18, 40 tall, 15 strong — waiting
    // while a picture is on its way. Its ink is the one for an accent
    // plate (egui's paints `ink`, the near-black the person turned down on
    // a teal button on 2026-11-10).
    let shutter_style = format!(
        "display: flex; align-items: center; gap: {}; min-height: {}; box-sizing: border-box; \
         padding: 0 {}; border-radius: {}; background: var(--m-accent); \
         color: var(--m-on-accent); font-size: {}; font-weight: 600; white-space: nowrap; \
         pointer-events: auto; {}",
        p.px(8.0),
        p.px(40.0),
        p.px(16.0),
        p.px(18.0),
        p.px(15.0),
        if v.shooting {
            "opacity: 0.5"
        } else {
            "cursor: pointer"
        }
    );
    let mut shutter = el("div")
        .attr("style", shutter_style)
        .attr("role", "button")
        .attr("aria-label", "Take it")
        .child(glyph(icons::CAMERA, 18.0, p))
        .child(el("span").child("Take it"));
    shutter = if v.shooting {
        // Its own node still listens, so a press on it is not the map's.
        shutter.on("click", |_: &Event| {})
    } else {
        shutter.on("click", on(acts, FinderAct::Shoot))
    };
    el("div")
        .attr("style", band)
        .child(button(icons::CLEAR, "Cancel", FinderAct::Cancel))
        .child(button(icons::CAM_FOLLOW, "Face me", FinderAct::FaceMe))
        .child(shutter)
        .into_view()
}
