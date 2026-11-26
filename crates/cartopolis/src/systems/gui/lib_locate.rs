//! The "you are here" arrow and its accuracy halo drawn by the component
//! library (`lib_host`), beside egui's (`locate::render_locate_arrow`).
//!
//! On with `CARTO_UI_LIB=locate` (or `1`), on the web `?ui=locate`. egui's
//! system keeps every decision — tracking on and not driving, where the fix
//! projects, culled to `place_pins::map_area` and the map's buttons, how
//! many screen points the reported accuracy spans (floored and clamped by
//! `locate::halo_radius`), which way the heading points on screen — and
//! publishes the frame in `LocateArrow::lib_view`; with `LocateArrow::lib`
//! on it paints nothing. Nothing here listens: the arrow answers a question
//! and offers no action, so a tap goes through it to the map.
//!
//! **One mount, restyled.** The arrow moves every frame the map does, so
//! nothing is rebuilt per frame: the clip, the spot, the halo's radius and
//! the kite's turn are each a signal read by one node's style, written only
//! once it has moved (half a pixel, a fifth of a degree). The kite's
//! triangles are built once, in a layer pointing up the screen, and the
//! layer is turned.
//!
//! **Built from boxes.** The halo is a disc and a bordered ring; the shadow
//! and the heading-less dot are discs. The kite is egui's two triangles
//! about the notch (`locate::KITE`), each a box of borders alone with one
//! side inked, turned so its longest side is its base (the apex then stands
//! over it, which a border triangle needs); a hairline down the shared
//! nose–notch edge hides the seam the two would leave, and the rim is four
//! thin boxes, one per edge, with square ends where egui's path is mitred.
//! Everything is clipped to the map's rect, as egui's painter is.

use std::cell::RefCell;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{Commands, Res, ResMut};
// The library's, not Bevy's.
use mithril::prelude::*;

use crate::systems::lib_pins::{css, px_scale};
use crate::systems::lib_settings::LibScreens;
use crate::systems::locate::{
    ArrowView, LocateArrow, MarkerLook, ARROW_HALF_LENGTH, DOT_R, DOT_RIM, HALO_FILL_ALPHA,
    HALO_RIM, HALO_RIM_ALPHA, KITE, KITE_RIM, SHADOW_R,
};
use crate::systems::theme;
use crate::systems::theme::tokens::hex;
use crate::systems::ui_input::UiMetrics;

/// The arrow's name in the library's host.
const KEY: &str = "locate";

/// The spot moves, and the halo grows, only once it has changed this much,
/// in points.
const MOVE_PT: f32 = 0.5;
/// The kite turns only once its heading has moved this much, degrees.
const TURN_DEG: f32 = 0.2;
/// Half the side of the layer the kite turns in, points: the kite with its
/// rim inside it.
const LAYER: f32 = ARROW_HALF_LENGTH + 2.0;
/// The contact shadow's ink: black at egui's `from_black_alpha(50)`.
const SHADOW_ALPHA: u8 = 50;

/// Which marker shows, without its turn (that is [`Live::turn`]).
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    None,
    Disc,
    Arrow,
}

#[derive(Clone)]
struct Live {
    /// The theme it was mounted in, and the library's px per egui point
    /// (`lib_pins::px_scale`): either changing mounts it again.
    dark: bool,
    k: f32,
    /// The map's rect as left, top, width, height, points; `None` hides all.
    clip: Signal<Option<[f32; 4]>>,
    /// The fix, points from the clip's corner.
    spot: Signal<(f32, f32)>,
    halo: Signal<Option<f32>>,
    shown: Signal<Shown>,
    /// The kite's turn, degrees clockwise from up the screen.
    turn: Signal<f32>,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Write a signal only when the value moved.
fn put<T: Clone + PartialEq + 'static>(s: &Signal<T>, v: T) {
    if s.peek(|now| *now != v) {
        s.set(v);
    }
}

/// Mounts the arrow once and mirrors egui's frame of it. `PostUpdate`,
/// before `DriveUi`, as every library screen; the arrow's bridge is
/// inserted here, the first frame.
pub fn drive_lib_locate(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    arrow: Option<ResMut<LocateArrow>>,
    metrics: bevy::prelude::Res<UiMetrics>,
    mut commands: Commands,
) {
    if !lib.locate {
        return;
    }
    let Some(mut arrow) = arrow else {
        commands.insert_resource(LocateArrow {
            lib: true,
            ..Default::default()
        });
        return;
    };
    if !arrow.lib {
        arrow.lib = true;
    }
    let view: Option<ArrowView> = arrow.lib_view;
    let k = px_scale(metrics.ui_scale());
    let dark = theme::p().ink == theme::DARK.ink;
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        if m.as_ref().is_some_and(|l| l.dark != dark || l.k != k) {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let live = m.get_or_insert_with(|| {
            let live = Live {
                dark,
                k,
                clip: signal(None),
                spot: signal((0.0, 0.0)),
                halo: signal(None),
                shown: signal(Shown::None),
                turn: signal(0.0),
            };
            crate::systems::lib_host::mount_keyed(KEY, view_of(&live), dark, theme::touch_layout());
            live
        });
        let Some(v) = view else {
            put(&live.clip, None);
            return;
        };
        put(
            &live.clip,
            Some([v.clip.min.x, v.clip.min.y, v.clip.width(), v.clip.height()]),
        );
        let spot = (v.here.x - v.clip.min.x, v.here.y - v.clip.min.y);
        let was = live.spot.get_untracked();
        if (was.0 - spot.0).abs() >= MOVE_PT || (was.1 - spot.1).abs() >= MOVE_PT {
            live.spot.set(spot);
        }
        let halo_moved = match (live.halo.get_untracked(), v.halo) {
            (Some(a), Some(b)) => (a - b).abs() >= MOVE_PT,
            (a, b) => a.is_some() != b.is_some(),
        };
        if halo_moved {
            live.halo.set(v.halo);
        }
        let shown = match v.marker {
            MarkerLook::None => Shown::None,
            MarkerLook::Disc => Shown::Disc,
            MarkerLook::Arrow { turn_deg } => {
                // The short way round, so 359.9° → 0.1° is a small move.
                let d = (turn_deg - live.turn.get_untracked() + 540.0).rem_euclid(360.0) - 180.0;
                if d.abs() >= TURN_DEG {
                    live.turn.set(turn_deg);
                }
                Shown::Arrow
            }
        };
        put(&live.shown, shown);
    });
}

/// px for points.
fn px(v: f32, k: f32) -> String {
    format!("{:.2}px", v * k)
}

/// An absolutely placed box in points: left, top, width, height, then the
/// rest. Nothing in it listens.
fn boxed(left: f32, top: f32, w: f32, h: f32, k: f32, rest: &str) -> String {
    format!(
        "position: absolute; left: {}; top: {}; width: {}; height: {}; pointer-events: none; {rest}",
        px(left, k),
        px(top, k),
        px(w, k),
        px(h, k)
    )
}

/// A disc of radius `r` about the spot, `rest` its paint.
fn disc_style(r: f32, k: f32, rest: &str) -> String {
    boxed(
        -r,
        -r,
        2.0 * r,
        2.0 * r,
        k,
        &format!(
            "box-sizing: border-box; border-radius: {}; {rest}",
            px(r, k)
        ),
    )
}

/// The clip over the map's rect, the spot on the fix inside it, and on the
/// spot the halo, the shadow, the dot and the turning kite.
fn view_of(l: &Live) -> View {
    let k = l.k;
    let p = theme::p();
    let accent = hex(p.accent);
    let cream = hex(p.cream);
    let wash = css(p.accent.with_alpha(HALO_FILL_ALPHA));
    let hairline = css(p.accent_ring.with_alpha_scaled(HALO_RIM_ALPHA));
    let shadow_ink = css(theme::Rgba8::BLACK.with_alpha(SHADOW_ALPHA));

    // Under every other library screen, as the pins are (`lib_pins`).
    let clip = {
        let c = l.clip.clone();
        move || match c.get() {
            Some([left, top, w, h]) => format!(
                "{}; overflow: hidden; z-index: -1",
                boxed(left, top, w, h, k, "")
            ),
            None => "display: none".to_string(),
        }
    };
    let spot = {
        let s = l.spot.clone();
        move || {
            let (x, y) = s.get();
            boxed(x, y, 0.0, 0.0, k, "")
        }
    };
    // The wash, then the hairline centred on the same radius (egui strokes
    // a circle across its edge; a CSS border lies inside the box).
    let halo_fill = {
        let h = l.halo.clone();
        let wash = wash.clone();
        move || match h.get() {
            Some(r) => disc_style(r, k, &format!("background: {wash}")),
            None => "display: none".to_string(),
        }
    };
    let halo_rim = {
        let h = l.halo.clone();
        let hairline = hairline.clone();
        move || match h.get() {
            Some(r) => disc_style(
                r + HALO_RIM / 2.0,
                k,
                &format!("border: {} solid {hairline}", px(HALO_RIM, k)),
            ),
            None => "display: none".to_string(),
        }
    };
    let shadow = {
        let s = l.shown.clone();
        let rest = format!("background: {shadow_ink}");
        move || match s.get() {
            Shown::None => "display: none".to_string(),
            _ => disc_style(SHADOW_R, k, &rest),
        }
    };
    let dot = {
        let s = l.shown.clone();
        let rest = format!(
            "background: {accent}; border: {} solid {cream}",
            px(DOT_RIM, k)
        );
        move || match s.get() {
            Shown::Disc => disc_style(DOT_R + DOT_RIM / 2.0, k, &rest),
            _ => "display: none".to_string(),
        }
    };
    let layer = {
        let (s, t) = (l.shown.clone(), l.turn.clone());
        move || match s.get() {
            Shown::Arrow => boxed(
                -LAYER,
                -LAYER,
                2.0 * LAYER,
                2.0 * LAYER,
                k,
                &format!("transform: rotate({:.1}deg)", t.get()),
            ),
            _ => "display: none".to_string(),
        }
    };

    el("div")
        .attr("style", clip)
        .child(
            el("div")
                .attr("style", spot)
                .child(el("div").attr("style", halo_fill))
                .child(el("div").attr("style", halo_rim))
                .child(el("div").attr("style", shadow))
                .child(el("div").attr("style", dot))
                .child(kite(el("div").attr("style", layer), &accent, &cream, k)),
        )
        .into_view()
}

/// A point in the kite's layer, points from its corner.
type Pt = (f32, f32);

/// `KITE`'s (along, across) the heading, in the layer pointing up the
/// screen: across is right, along is up.
fn in_layer((along, across): (f32, f32)) -> Pt {
    (LAYER + across, LAYER - along)
}

/// The kite in its layer: the two triangles about the notch, the hairline
/// over the edge they share, and the rim round the outline.
fn kite(layer: mithril::view::El, accent: &str, cream: &str, k: f32) -> View {
    let [nose, left, notch, right] = KITE.map(in_layer);
    // Down the nose–notch edge, from where the kite is wider than the
    // hairline: nearer the nose it would show past the outline.
    let seam_top = in_layer((ARROW_HALF_LENGTH - 2.0, 0.0)).1;
    let seam = boxed(
        LAYER - 0.5,
        seam_top,
        1.0,
        notch.1 - seam_top,
        k,
        &format!("background: {accent}"),
    );
    let mut layer = layer
        .child(triangle([nose, left, notch], accent, k))
        .child(triangle([nose, notch, right], accent, k))
        .child(el("div").attr("style", seam));
    for (a, b) in [(nose, left), (left, notch), (notch, right), (right, nose)] {
        layer = layer.child(segment(a, b, KITE_RIM, cream, k));
    }
    layer.into_view()
}

/// A filled triangle: a box of borders alone, three clear and the bottom
/// inked, which CSS (and `bevy_ui`'s border shader) splits on the diagonals
/// into a triangle on the box's bottom edge with its apex on the top one;
/// turned so the bottom edge is the triangle's longest side.
fn triangle(pts: [Pt; 3], ink: &str, k: f32) -> View {
    let dist = |a: Pt, b: Pt| (b.0 - a.0).hypot(b.1 - a.1);
    // The longest side as the base: the apex then stands over it.
    let mut best = (pts[0], pts[1], pts[2]);
    for (a, b, c) in [(pts[1], pts[2], pts[0]), (pts[2], pts[0], pts[1])] {
        if dist(a, b) > dist(best.0, best.1) {
            best = (a, b, c);
        }
    }
    let (a, b, apex) = best;
    let base = dist(a, b);
    if base <= f32::EPSILON {
        return View::Empty;
    }
    // Along the base, and the base's normal: the box's right and up.
    let u0 = ((b.0 - a.0) / base, (b.1 - a.1) / base);
    let n0 = (u0.1, -u0.0);
    let h0 = (apex.0 - a.0) * n0.0 + (apex.1 - a.1) * n0.1;
    // The apex must be on the box's up side: else start from the other end.
    let (from, u, n, h) = if h0 >= 0.0 {
        (a, u0, n0, h0)
    } else {
        (b, (-u0.0, -u0.1), (-n0.0, -n0.1), -h0)
    };
    let s = (apex.0 - from.0) * u.0 + (apex.1 - from.1) * u.1;
    // `rotate` turns about the box's middle, which lies half the base along
    // and half the height up from `from`.
    let cx = from.0 + u.0 * base / 2.0 + n.0 * h / 2.0;
    let cy = from.1 + u.1 * base / 2.0 + n.1 * h / 2.0;
    let deg = u.1.atan2(u.0).to_degrees();
    let style = boxed(
        cx - base / 2.0,
        cy - h / 2.0,
        base,
        h,
        k,
        &format!(
            "box-sizing: border-box; border-style: solid; border-width: 0 {} {} {}; \
             border-color: transparent; border-bottom-color: {ink}; transform: rotate({deg:.2}deg)",
            px(base - s, k),
            px(h, k),
            px(s, k),
        ),
    );
    el("div").attr("style", style).into_view()
}

/// A stroke from `a` to `b`, `width` across, with square ends half its
/// width past each point: a thin box turned onto the line.
fn segment(a: Pt, b: Pt, width: f32, ink: &str, k: f32) -> View {
    let len = (b.0 - a.0).hypot(b.1 - a.1) + width;
    let (mx, my) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let deg = (b.1 - a.1).atan2(b.0 - a.0).to_degrees();
    let style = boxed(
        mx - len / 2.0,
        my - width / 2.0,
        len,
        width,
        k,
        &format!("background: {ink}; transform: rotate({deg:.2}deg)"),
    );
    el("div").attr("style", style).into_view()
}
