//! What lies past the globe's ceiling drawn by the component library
//! (`lib_host`), beside egui's (`cosmos::render_cosmos`): the Sun, the
//! planets and their orbits, the Earth and the Moon lit by the real Sun, the
//! nearest stars and the lanes between them, the light-travel rings, the
//! edge of the observable universe, and every label and caption.
//!
//! On with `CARTO_UI_LIB=cosmos` (or `1`). egui's system keeps all of the
//! maths — the projection and its squeeze, the fades, which labels find
//! room — and with `CosmosLib::lib` on paints at most the space round the
//! globe's hole and the two star fields (below), publishing the rest of its
//! frame in `CosmosLib::lib_view`. This mounts while that is `Some` and clears
//! when it is `None` (below the ceiling).
//!
//! **One mount, restyled.** Every shape is one absolutely placed box and
//! every label one text box, each keyed (`"planet 3"`, `"ring 1 light
//! year"`, a label by its words) with its style in a signal: a frame in
//! which the view moved rewrites the styles that changed and mounts
//! nothing. A key not in the frame is hidden, not unmounted; the list is
//! rebuilt only when a key is seen for the first time, or the hidden are
//! swept.
//!
//! **Built from boxes.** A disc is a `border-radius` box; a ring a bordered
//! one; an orbit a bordered box as wide and high as egui's squeezed circle
//! is across its two axes, turned by one `rotate()`; a lane a 1 px box
//! turned onto its line. A lit sphere is one disc and one gradient: the
//! terminator is egui's ellipse, which a circle through the same three
//! points stands in for (through the two limb points across the Sun's
//! direction and the point on that axis), lit inside it for a gibbous
//! phase and outside it for a crescent — a hard edge, as egui's.
//!
//! **Not boxes**: the space behind everything (a full-screen fill with the
//! globe's round hole, under the star fields), the Milky Way's ~7,000
//! points and the galaxies' up to 50,000 — a node each would be the
//! heaviest screen the library has, for one-pixel squares. On the Bevy
//! backend `lib_cosmos_sky` draws them as two meshes on the library's own
//! camera, under its UI; as DOM (the web) there is no such camera, and they
//! stay painted on egui's background layer, under the DOM.
//!
//! What differs: egui's halo is four offset copies of the text in the dark
//! theme's ink, and so is this one's. On the Bevy backend a border radius
//! is round, never elliptical, so an orbit seen at a slant comes out a
//! rounded oblong rather than an ellipse (the DOM draws the ellipse);
//! orbits are drawn only once the view looks down on them (egui's
//! `face_on`), where they are close to round.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
use egui::Color32;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;

use crate::systems::cosmos::{CosmosLib, CosmosShape, CosmosTag, ShapeKind};
use crate::systems::lib_pins::{boxed, px_scale};
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme;
use crate::systems::theme::tokens::hex;
use crate::systems::ui_input::UiMetrics;

/// The cosmos' name in the library's host.
const KEY: &str = "cosmos";

/// A box not in this frame.
const HIDDEN: &str = "display: none";

/// Under this the box is not drawn at all.
const FAINT: f32 = 0.004;

/// Keys not in the frame are kept hidden, so coming back costs a restyle
/// and not a remount, until there are more than this many.
const MAX_HIDDEN: usize = 64;

/// How far out a terminator's circle may have its centre, as a share of the
/// sphere's radius: near a half phase the terminator's circle grows without
/// bound towards egui's straight line.
const TERMINATOR_REACH: f32 = 8.0;

// ─── A keyed list of restyled boxes ─────────────────────────────────────────

/// One box: its style, and its words if it is a label (fixed: a label's
/// key is its words).
#[derive(Clone)]
struct Slot {
    style: Signal<String>,
    text: String,
}

/// Boxes in one mount, keyed, each with its style in a signal.
#[derive(Clone)]
struct Boxes {
    keys: Signal<Vec<String>>,
    slots: Rc<RefCell<HashMap<String, Slot>>>,
}

impl Boxes {
    fn new() -> Self {
        Boxes {
            keys: signal(Vec::new()),
            slots: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// This frame's boxes: key, style, words. A box missing from them is
    /// hidden. Writes only the styles that changed.
    fn sync(&self, items: impl IntoIterator<Item = (String, String, String)>) {
        let mut writes: Vec<(Signal<String>, String)> = Vec::new();
        let mut added: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let swept = {
            let mut slots = self.slots.borrow_mut();
            for (key, style, text) in items {
                if !seen.insert(key.clone()) {
                    continue;
                }
                match slots.get(&key) {
                    Some(slot) => {
                        if slot.style.get_untracked() != style {
                            writes.push((slot.style.clone(), style));
                        }
                    }
                    None => {
                        slots.insert(
                            key.clone(),
                            Slot {
                                style: signal(style),
                                text,
                            },
                        );
                        added.push(key);
                    }
                }
            }
            let mut hidden = 0usize;
            for (key, slot) in slots.iter() {
                if seen.contains(key) {
                    continue;
                }
                hidden += 1;
                if slot.style.get_untracked() != HIDDEN {
                    writes.push((slot.style.clone(), HIDDEN.to_string()));
                }
            }
            let swept = hidden > MAX_HIDDEN;
            if swept {
                slots.retain(|k, _| seen.contains(k));
            }
            swept
        };
        // Written with the list released: a write runs the views that read
        // it, and the list's own view reads the list.
        for (style, value) in writes {
            style.set(value);
        }
        if !added.is_empty() || swept {
            let mut keys = self.keys.get_untracked();
            if swept {
                keys.retain(|k| seen.contains(k));
            }
            keys.extend(added);
            self.keys.set(keys);
        }
    }

    /// The list: a 0 × 0 node at the root's corner, stacked `z` among the
    /// cosmos' lists, a box per key placed from there.
    fn view(&self, z: i32, labels: bool) -> View {
        let keys = self.keys.clone();
        let slots = self.slots.clone();
        let list = move || -> View {
            let keys = keys.get();
            let items: Vec<Slot> = {
                let slots = slots.borrow();
                keys.iter().filter_map(|k| slots.get(k).cloned()).collect()
            };
            View::Fragment(
                items
                    .into_iter()
                    .map(|slot| {
                        if labels {
                            label_box(slot)
                        } else {
                            shape_box(slot)
                        }
                    })
                    .collect(),
            )
        };
        let frame =
            format!("position: absolute; left: 0; top: 0; width: 0; height: 0; z-index: {z}");
        view! {
            <div style={frame}>{list}</div>
        }
    }
}

fn shape_box(slot: Slot) -> View {
    let style = slot.style;
    let style = move || style.get();
    view! { <div style={style}></div> }
}

/// A label: its words over four copies one pixel off in the dark theme's
/// ink, egui's halo, so it reads over anything.
fn label_box(slot: Slot) -> View {
    let style = slot.style;
    let style = move || style.get();
    let ink = hex(theme::DARK.ink);
    let halo = |dx: i32, dy: i32| {
        format!("position: absolute; left: {dx}px; top: {dy}px; white-space: nowrap; color: {ink}")
    };
    let (h1, h2, h3, h4) = (halo(1, 0), halo(-1, 0), halo(0, 1), halo(0, -1));
    let text = slot.text;
    let (t1, t2, t3, t4) = (text.clone(), text.clone(), text.clone(), text.clone());
    view! {
        <div style={style}>
            <div style={h1}>{t1}</div>
            <div style={h2}>{t2}</div>
            <div style={h3}>{t3}</div>
            <div style={h4}>{t4}</div>
            <div style="position: relative; white-space: nowrap">{text}</div>
        </div>
    }
}

// ─── The cosmos ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Live {
    shapes: Boxes,
    labels: Boxes,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts the cosmos while egui publishes a frame of it, restyles it to
/// each frame, and clears it below the ceiling. `PostUpdate`, before
/// `DriveUi`, as every library screen; the cosmos' state is inserted here,
/// the first frame.
pub fn drive_lib_cosmos(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    cosmos: Option<ResMut<CosmosLib>>,
    metrics: Res<UiMetrics>,
    mut commands: Commands,
) {
    if !lib.cosmos {
        return;
    }
    let Some(mut cosmos) = cosmos else {
        commands.insert_resource(CosmosLib {
            lib: true,
            ..Default::default()
        });
        return;
    };
    if !cosmos.lib {
        cosmos.lib = true;
    }
    let k = px_scale(metrics.ui_scale());
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        let Some(frame) = cosmos.lib_view.as_ref() else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        };
        let live = m.get_or_insert_with(|| {
            let live = Live {
                shapes: Boxes::new(),
                labels: Boxes::new(),
            };
            crate::systems::lib_host::mount_keyed(
                KEY,
                view_of(&live),
                theme::p().ink == theme::DARK.ink,
                theme::touch_layout(),
            );
            live
        });
        live.shapes.sync(
            frame
                .shapes
                .iter()
                .enumerate()
                .map(|(z, s)| (s.key.clone(), shape_style(s, z, k), String::new())),
        );
        // A label's key is its words; the same words twice get a count.
        let mut count: HashMap<&str, usize> = HashMap::new();
        live.labels
            .sync(frame.tags.iter().enumerate().map(|(z, t)| {
                let n = count.entry(t.text.as_str()).or_insert(0);
                let key = if *n == 0 {
                    t.text.clone()
                } else {
                    format!("{}#{n}", t.text)
                };
                *n += 1;
                (key, label_style(t, z, k), t.text.clone())
            }));
    });
}

/// The root: the whole window's corner, under every other library screen
/// (the map's names at -2 are the lowest of those), listening nowhere: a
/// drag over the cosmos moves the map. The shapes, then the labels over
/// them, as egui paints them.
fn view_of(l: &Live) -> View {
    let shapes = l.shapes.view(0, false);
    let labels = l.labels.view(1, true);
    view! {
        <div style="position: absolute; left: 0; top: 0; width: 0; height: 0; z-index: -20; pointer-events: none">
            {shapes}
            {labels}
        </div>
    }
}

/// An egui colour as CSS, opaque, and its own alpha (`ORBIT`, `LANE` and
/// `RING` carry one), which goes into the box's opacity.
fn solid(c: Color32) -> (String, f32) {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    (hex(theme::Rgba8::from_rgb(r, g, b)), f32::from(a) / 255.0)
}

fn finite(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// A border radius that makes a box `w` × `h` round: an ellipse as DOM,
/// and on the Bevy backend, whose radii are round, the most it can.
fn round(w: f32, h: f32) -> String {
    if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        "50%".to_string()
    } else {
        format!("{:.1}px", w.max(h) / 2.0)
    }
}

/// One shape's box, stacked `z` in egui's paint order.
fn shape_style(s: &CosmosShape, z: usize, k: f32) -> String {
    let (body, ink_alpha, ok) = match &s.kind {
        ShapeKind::Disc { c, r, ink } => {
            let (fill, a) = solid(*ink);
            let r = r * k;
            (
                boxed(
                    c.x * k - r,
                    c.y * k - r,
                    2.0 * r,
                    2.0 * r,
                    &format!("border-radius: {r:.2}px; background: {fill}"),
                ),
                a,
                finite(&[c.x, c.y, r]),
            )
        }
        ShapeKind::Ring { c, r, width, ink } => {
            let (line, a) = solid(*ink);
            let w = width * k;
            let outer = r * k + w / 2.0;
            (
                boxed(
                    c.x * k - outer,
                    c.y * k - outer,
                    2.0 * outer,
                    2.0 * outer,
                    &format!(
                        "box-sizing: border-box; border: {w:.1}px solid {line}; border-radius: {outer:.1}px"
                    ),
                ),
                a,
                finite(&[c.x, c.y, outer]),
            )
        }
        ShapeKind::Ellipse {
            c,
            half,
            angle_deg,
            ink,
        } => {
            let (line, a) = solid(*ink);
            let w = k;
            let (hw, hh) = (half.x * k + w / 2.0, half.y * k + w / 2.0);
            (
                boxed(
                    c.x * k - hw,
                    c.y * k - hh,
                    2.0 * hw,
                    2.0 * hh,
                    &format!(
                        "box-sizing: border-box; border: {w:.1}px solid {line}; border-radius: {}; transform: rotate({angle_deg:.1}deg)",
                        round(2.0 * hw, 2.0 * hh)
                    ),
                ),
                a,
                finite(&[c.x, c.y, hw, hh, *angle_deg]),
            )
        }
        ShapeKind::Line {
            a: from,
            b: to,
            ink,
        } => {
            let (fill, a) = solid(*ink);
            let d = (*to - *from) * k;
            let len = d.length();
            let (mx, my) = ((from.x + to.x) / 2.0 * k, (from.y + to.y) / 2.0 * k);
            let deg = d.y.atan2(d.x).to_degrees();
            let w = k;
            (
                boxed(
                    mx - len / 2.0,
                    my - w / 2.0,
                    len,
                    w,
                    &format!("background: {fill}; transform: rotate({deg:.1}deg)"),
                ),
                a,
                finite(&[mx, my, len, deg]),
            )
        }
        ShapeKind::Sphere {
            c,
            r,
            sun,
            toward,
            day,
            night,
        } => (
            sphere(*c, *r, *sun, *toward, *day, *night, k),
            solid(*day).1,
            finite(&[c.x, c.y, *r, sun.x, sun.y, *toward]),
        ),
    };
    let o = (ink_alpha * s.alpha).clamp(0.0, 1.0);
    if !ok || o.is_nan() || o < FAINT {
        return HIDDEN.to_string();
    }
    format!("{body}; opacity: {o:.3}; z-index: {z}")
}

/// A sphere seen from the camera, lit from one side (`cosmos::paint_lit_sphere`
/// draws the same with a mesh). `sun` is the Sun's direction on screen, x
/// right and y up; `toward` how far it stands towards the viewer, −1 behind
/// the sphere to 1 behind the viewer.
///
/// egui's lit part runs from the limb on the Sun's side to the terminator,
/// an ellipse through the two limb points across the Sun's direction and
/// the point `−toward · r` along it. The circle through those three points
/// has its centre `x0 = r(1 − t²)/2t` along the Sun's direction: for a
/// gibbous phase (t > 0) the lit part is inside it, for a crescent outside.
fn sphere(
    c: egui::Pos2,
    r: f32,
    sun: Vec2,
    toward: f32,
    day: Color32,
    night: Color32,
    k: f32,
) -> String {
    let (day, night) = (solid(day).0, solid(night).0);
    if r < 1.5 {
        // Too small for a phase: egui's day over its night, as one dot.
        let r = r.max(1.0) * k;
        return boxed(
            c.x * k - r,
            c.y * k - r,
            2.0 * r,
            2.0 * r,
            &format!("border-radius: {r:.2}px; background: {day}"),
        );
    }
    let rk = r * k;
    let d = 2.0 * rk;
    let u = if sun.length_squared() > 1e-8 {
        sun.normalize()
    } else {
        Vec2::X
    };
    // Screen y runs down.
    let (ux, uy) = (u.x, -u.y);
    let t = toward.clamp(-1.0, 1.0);
    let fill = if t.abs() < 0.02 {
        // Half lit: the terminator is a straight line through the middle.
        // CSS's angle is clockwise from up, towards the Sun here.
        let deg = ux.atan2(-uy).to_degrees();
        format!("linear-gradient({deg:.1}deg, {night} 49.5%, {day} 50.5%)")
    } else {
        let x0 =
            (rk * (1.0 - t * t) / (2.0 * t)).clamp(-TERMINATOR_REACH * rk, TERMINATOR_REACH * rk);
        let big = (x0 * x0 + rk * rk).sqrt();
        let (cx, cy) = (50.0 + 100.0 * x0 * ux / d, 50.0 + 100.0 * x0 * uy / d);
        let (inside, outside) = if t > 0.0 {
            (&day, &night)
        } else {
            (&night, &day)
        };
        format!(
            "radial-gradient(circle farthest-side at {cx:.1}% {cy:.1}%, {inside} {:.1}px, {outside} {:.1}px)",
            (big - 0.5).max(0.0),
            big + 0.5
        )
    };
    boxed(
        c.x * k - rk,
        c.y * k - rk,
        d,
        d,
        &format!("border-radius: {rk:.2}px; background-image: {fill}"),
    )
}

/// One label's box where egui measured it, its line as tall as egui's.
fn label_style(t: &CosmosTag, z: usize, k: f32) -> String {
    let (ink, a) = solid(t.ink);
    let o = (a * t.alpha).clamp(0.0, 1.0);
    if o.is_nan() || o < FAINT || !finite(&[t.min.x, t.min.y, t.size.y, t.font]) || t.font <= 0.0 {
        return HIDDEN.to_string();
    }
    format!(
        "position: absolute; left: {:.1}px; top: {:.1}px; white-space: nowrap; font-size: {:.1}px; line-height: {:.3}; color: {ink}; opacity: {o:.3}; z-index: {z}",
        t.min.x * k,
        t.min.y * k,
        t.font * k,
        t.size.y / t.font
    )
}
