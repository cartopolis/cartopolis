//! A lens's marks over the map drawn by the component library (`lib_host`):
//! the outline of the place being read and the card beside the pointer, a
//! scenario's tracks — the cone of where a group may go, its path behind
//! and (dashed) ahead, a time every half hour, where some were brought
//! down, and the head with its chip — and the mode's frame, the line round
//! the whole screen in the mode's colour, striped for an exercise
//! (`docs/notes/scenarios.md`).
//!
//! `scenario::place_scenario_map` projects everything and publishes it as
//! screen points (`ScenarioView::map`); the frame is
//! `ScenarioView::mode_frame`. This mounts while a lens is open and clears
//! when it closes. Nothing here listens, so a press goes through to the map
//! (as DOM by `pointer-events: none`).
//!
//! **One mount, restyled.** The marks move with the map every frame, so
//! they are pools of boxes, one for shapes and one for words, whose items
//! each hold their style (and words) in a signal: a frame restyles the
//! boxes that changed, the pool grows only when a frame needs more boxes
//! than any before, and boxes a frame does not use are hidden, not
//! unmounted.
//!
//! **Built from boxes.** A line is a run of thin boxes, each turned onto
//! its segment by one `rotate()`; an opaque one has round ends carried half
//! a width past its points, so the joints show no notch, a translucent one
//! square ends that do not overlap (an overlap would show darker beads).
//! A dashed line is its dashes, each a box. A disc is a `border-radius`
//! box; the card and the chip are boxes with text. The marks are clipped to
//! the map's width by their container's `overflow: hidden`.
//!
//! What differs from the painted marks before them: the cone is a box per
//! five-minute step, each as wide as the cone halfway along it, so its edge
//! is a fine stair where a mesh was smooth (the 1 px edge line is drawn over
//! it as it was); lines drop points closer than [`MIN_STEP`] together.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{Res, Vec2};
use mithril::prelude::*;

use crate::systems::lib_pins::{boxed, css, px_scale};
use crate::systems::lib_settings::LibScreens;
use crate::systems::scenario::{LensMapView, ModeChip, ScenarioView};
use crate::systems::theme::{self, scenario as ink};
use crate::systems::ui_input::UiMetrics;

/// The marks' name in the library's host.
const KEY: &str = "scenario_map";

/// A box not in this frame.
const HIDDEN: &str = "display: none";

/// Under the place names (-2) and the pins (-1): a city stays readable over
/// a coloured province and a saved place on top of a track.
const Z_MARKS: i32 = -3;
/// The card over the pointer, over the panels, as a tooltip is.
const Z_TIP: i32 = 800;
/// The mode's frame, over everything but the card's peers above it.
const Z_FRAME: i32 = 850;

/// Points of a line closer together than this, px, are drawn as one: an
/// outline at a country's scale has thousands of points under a pixel apart.
const MIN_STEP: f32 = 2.0;
/// Most boxes one frame's marks may take.
const MAX_BOXES: usize = 3000;
/// Boxes a pool keeps hidden past what a frame uses before it sheds them.
const SPARE: usize = 256;

/// The exercise frame's stripes: their pitch and width, px.
const STRIPE_STEP: f32 = 12.0;
const STRIPE_W: f32 = 4.0;

/// A label chip's padding, px (`theme::CHIP_PADDING`).
const CHIP_PAD: (f32, f32) = (6.0, 3.0);

// ─── A pool of restyled boxes ───────────────────────────────────────────────

/// One box: its style, and its words (empty for a shape).
#[derive(Clone)]
struct Item {
    style: Signal<String>,
    text: Signal<String>,
}

/// Boxes in one mount, by index, each with its style and words in signals.
#[derive(Clone)]
struct Pool {
    len: Signal<usize>,
    items: Rc<RefCell<Vec<Item>>>,
}

impl Pool {
    fn new() -> Self {
        Pool {
            len: signal(0),
            items: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// This frame's boxes, style and words, in the order they stack (the
    /// last on top). The rest are hidden. Writes only what changed.
    fn sync(&self, next: Vec<(String, String)>) {
        let shown = next.len();
        let mut writes: Vec<(Signal<String>, String)> = Vec::new();
        let resized = {
            let mut items = self.items.borrow_mut();
            let had = items.len();
            for (i, (style, text)) in next.into_iter().enumerate() {
                match items.get(i) {
                    Some(it) => {
                        if it.style.peek(|now| *now != style) {
                            writes.push((it.style.clone(), style));
                        }
                        if it.text.peek(|now| *now != text) {
                            writes.push((it.text.clone(), text));
                        }
                    }
                    None => items.push(Item {
                        style: signal(style),
                        text: signal(text),
                    }),
                }
            }
            // Grown and shed a [`SPARE`] at a time: a new length remounts
            // every box, and a zoom that adds a few each frame would
            // otherwise remount them all every frame.
            if items.len() > had {
                let room = items.len().next_multiple_of(SPARE);
                items.resize_with(room, || Item {
                    style: signal(HIDDEN.to_string()),
                    text: signal(String::new()),
                });
            }
            if items.len() > shown + 2 * SPARE {
                items.truncate(shown.next_multiple_of(SPARE));
            }
            for it in items.iter().skip(shown) {
                if it.style.peek(|now| now.as_str() != HIDDEN) {
                    writes.push((it.style.clone(), HIDDEN.to_string()));
                }
            }
            (items.len() != had).then_some(items.len())
        };
        // Written with the pool released: a write runs the views that read
        // it, and the pool's own view reads the pool.
        for (s, v) in writes {
            s.set(v);
        }
        if let Some(n) = resized {
            self.len.set(n);
        }
    }

    /// The pool: a 0 × 0 node at its container's corner, a box per item
    /// placed from there.
    fn view(&self, words: bool) -> View {
        let len = self.len.clone();
        let items = self.items.clone();
        let list = move || -> View {
            let n = len.get();
            let shown: Vec<Item> = items.borrow().iter().take(n).cloned().collect();
            View::Fragment(
                shown
                    .into_iter()
                    .map(|it| {
                        let style = it.style;
                        let style = move || style.get();
                        if words {
                            let text = it.text;
                            let text = move || text.get();
                            view! { <div style={style}>{text}</div> }
                        } else {
                            view! { <div style={style}></div> }
                        }
                    })
                    .collect(),
            )
        };
        view! {
            <div style="position: absolute; left: 0; top: 0; width: 0; height: 0">{list}</div>
        }
    }
}

/// Write a signal only when the value moved: a write re-runs what reads it.
fn put<T: Clone + PartialEq + 'static>(s: &Signal<T>, v: T) {
    if s.peek(|now| *now != v) {
        s.set(v);
    }
}

// ─── Shapes from boxes ──────────────────────────────────────────────────────

/// A stroke from `a` to `b`, `w` wide, turned onto its segment. Round: its
/// ends rounded and carried half a width past the points, for an opaque
/// line's joints; else square and exact, for a translucent one.
fn seg(a: Vec2, b: Vec2, w: f32, ink: &str, round: bool) -> Option<String> {
    let d = b - a;
    let len = d.length();
    if len < 0.01 {
        return None;
    }
    let (len, radius) = if round {
        (len + w, format!("border-radius: {:.2}px; ", w * 0.5))
    } else {
        (len, String::new())
    };
    let deg = d.y.atan2(d.x).to_degrees();
    let m = (a + b) * 0.5;
    Some(boxed(
        m.x - len * 0.5,
        m.y - w * 0.5,
        len,
        w,
        &format!("{radius}background: {ink}; transform: rotate({deg:.2}deg)"),
    ))
}

/// A filled disc of radius `r` at `c`.
fn disc(c: Vec2, r: f32, ink: &str) -> String {
    boxed(
        c.x - r,
        c.y - r,
        2.0 * r,
        2.0 * r,
        &format!("border-radius: {r:.2}px; background: {ink}"),
    )
}

/// A run's points less those closer than [`MIN_STEP`] to the last one kept;
/// its last point always stays.
fn thinned(run: &[Vec2]) -> Vec<Vec2> {
    let mut out: Vec<Vec2> = Vec::with_capacity(run.len());
    for &p in run {
        match out.last() {
            Some(&q) if q.distance(p) < MIN_STEP => {}
            _ => out.push(p),
        }
    }
    if let (Some(&last), Some(&kept)) = (run.last(), out.last()) {
        if kept != last {
            out.push(last);
        }
    }
    out
}

/// The parts of a run with a segment in the box from the origin to `size`,
/// grown by `margin`: a place's whole outline, read up close, is thousands
/// of boxes off screen, each restyled every frame the map moves.
fn on_screen(run: &[Vec2], size: Vec2, margin: f32) -> Vec<Vec<Vec2>> {
    let (lo, hi) = (Vec2::splat(-margin), size + margin);
    let mut parts: Vec<Vec<Vec2>> = Vec::new();
    let mut open = false;
    for pair in run.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let shown = a.max(b).cmpge(lo).all() && a.min(b).cmple(hi).all();
        match (shown, open) {
            (true, true) => parts.last_mut().unwrap().push(b),
            (true, false) => parts.push(vec![a, b]),
            _ => {}
        }
        open = shown;
    }
    parts
}

/// A line through a run of points, as a box per segment.
fn line(run: &[Vec2], w: f32, ink: &str, round: bool, out: &mut Vec<(String, String)>) {
    for pair in thinned(run).windows(2) {
        if let Some(s) = seg(pair[0], pair[1], w, ink, round) {
            out.push((s, String::new()));
        }
    }
}

/// A dashed line through a run of points: `dash` on, `gap` off, from its
/// start.
fn dashed(run: &[Vec2], w: f32, ink: &str, dash: f32, gap: f32, out: &mut Vec<(String, String)>) {
    let (mut on, mut left) = (true, dash);
    for pair in thinned(run).windows(2) {
        let (mut p, b) = (pair[0], pair[1]);
        let mut rest = p.distance(b);
        if rest < 0.01 {
            continue;
        }
        let dir = (b - p) / rest;
        while rest > 1e-4 {
            let step = rest.min(left);
            let q = p + dir * step;
            if on {
                if let Some(s) = seg(p, q, w, ink, false) {
                    out.push((s, String::new()));
                }
            }
            p = q;
            rest -= step;
            left -= step;
            if left <= 1e-4 {
                on = !on;
                left = if on { dash } else { gap };
            }
        }
    }
}

// ─── The marks ──────────────────────────────────────────────────────────────

/// The card's words and swatch.
#[derive(Clone, PartialEq)]
struct TipBody {
    name: String,
    value: String,
    band: String,
    colour: [u8; 3],
}

/// One edge of an exercise's frame: a clipping box and its stripes.
#[derive(Clone)]
struct Band {
    style: Signal<String>,
    stripes: Pool,
}

#[derive(Clone)]
struct Live {
    /// The marks' clipping box: the map's width, the screen's height.
    marks: Signal<String>,
    shapes: Pool,
    words: Pool,
    tip_at: Signal<String>,
    tip: Signal<Option<TipBody>>,
    /// The frame's bordered box.
    frame: Signal<String>,
    /// An exercise's striped edges: top, bottom, left, right.
    bands: Vec<Band>,
    /// The frame last drawn, and the screen it was drawn for (px).
    framed: Rc<RefCell<Option<(ModeChip, Vec2)>>>,
    /// The theme it was mounted in: another remounts it.
    dark: bool,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

impl Live {
    fn new(dark: bool) -> Self {
        Live {
            marks: signal(HIDDEN.to_string()),
            shapes: Pool::new(),
            words: Pool::new(),
            tip_at: signal(HIDDEN.to_string()),
            tip: signal(None),
            frame: signal(HIDDEN.to_string()),
            bands: (0..4)
                .map(|_| Band {
                    style: signal(HIDDEN.to_string()),
                    stripes: Pool::new(),
                })
                .collect(),
            framed: Rc::new(RefCell::new(None)),
            dark,
        }
    }

    /// The line round the whole screen in the mode's colour; an exercise's
    /// is the ink, twice as wide, with the colour's stripes across it.
    /// `screen` in px. Redrawn only when the mode or the screen changed.
    fn frame_to(&self, mode: ModeChip, screen: Vec2) {
        if *self.framed.borrow() == Some((mode, screen)) {
            return;
        }
        *self.framed.borrow_mut() = Some((mode, screen));
        let band = if mode.striped { 6.0 } else { 3.0 };
        let edge = if mode.striped {
            css(theme::p().ink)
        } else {
            css(mode.colour)
        };
        put(
            &self.frame,
            format!(
                "position: absolute; left: 0; top: 0; right: 0; bottom: 0; \
                 border: {band}px solid {edge}; pointer-events: none; z-index: {Z_FRAME}"
            ),
        );
        let colour = css(mode.colour);
        let edges = [
            ("left: 0; top: 0", screen.x, band),
            ("left: 0; bottom: 0", screen.x, band),
            ("left: 0; top: 0", band, screen.y),
            ("right: 0; top: 0", band, screen.y),
        ];
        for (b, (at, w, h)) in self.bands.iter().zip(edges) {
            if !mode.striped {
                put(&b.style, HIDDEN.to_string());
                b.stripes.sync(Vec::new());
                continue;
            }
            put(
                &b.style,
                format!(
                    "position: absolute; {at}; width: {w:.1}px; height: {h:.1}px; \
                     overflow: hidden; pointer-events: none; z-index: {}",
                    Z_FRAME + 1
                ),
            );
            // From the edge's bottom up to its top, a height to the right.
            let mut stripes = Vec::new();
            let mut x = -h;
            while x < w {
                if let Some(s) = seg(
                    Vec2::new(x, h),
                    Vec2::new(x + h, 0.0),
                    STRIPE_W,
                    &colour,
                    false,
                ) {
                    stripes.push((s, String::new()));
                }
                x += STRIPE_STEP;
            }
            b.stripes.sync(stripes);
        }
    }

    /// The frame's marks and the card, from the published points (UI points;
    /// `k` px each).
    fn marks_to(&self, v: &LensMapView, k: f32) {
        match &v.tip {
            Some(t) => {
                let p = theme::p();
                put(
                    &self.tip_at,
                    format!(
                        "position: absolute; left: {:.1}px; top: {:.1}px; max-width: 220px; \
                         padding: 12px 14px; background: {}; border: 1px solid {}; \
                         border-radius: {}px; pointer-events: none; z-index: {Z_TIP}",
                        t.at.x * k + 18.0,
                        t.at.y * k + 18.0,
                        css(p.overlay),
                        css(p.overlay_line),
                        theme::RADIUS_LG
                    ),
                );
                put(
                    &self.tip,
                    Some(TipBody {
                        name: t.name.clone(),
                        value: t.value.clone(),
                        band: t.band.clone(),
                        colour: t.colour,
                    }),
                );
            }
            None => {
                put(&self.tip_at, HIDDEN.to_string());
                put(&self.tip, None);
            }
        }
        let Some(clip) = v.clip else {
            put(&self.marks, HIDDEN.to_string());
            self.shapes.sync(Vec::new());
            self.words.sync(Vec::new());
            return;
        };
        put(
            &self.marks,
            boxed(
                clip.min.x * k,
                clip.min.y * k,
                clip.width() * k,
                clip.height() * k,
                &format!("overflow: hidden; pointer-events: none; z-index: {Z_MARKS}"),
            ),
        );
        // Points in the clipping box, px.
        let at = |p: Vec2| (p - clip.min) * k;
        let size = clip.size() * k;
        // Only what is in the box, less a margin past a line's widest.
        let runs = |r: &Vec<Vec2>| -> Vec<Vec<Vec2>> {
            let r: Vec<Vec2> = r.iter().map(|&p| at(p)).collect();
            on_screen(&r, size, 8.0)
        };
        let mut shapes: Vec<(String, String)> = Vec::new();
        let mut words: Vec<(String, String)> = Vec::new();

        // The place being read: a dark halo under a light line.
        let (halo, cream) = (css(ink::AREA_HALO), css(theme::p().cream));
        for r in v.outline.iter().flat_map(&runs) {
            line(&r, 4.5, &halo, false, &mut shapes);
            line(&r, 2.0, &cream, true, &mut shapes);
        }

        let (cone, cone_edge) = (css(ink::CONE), css(ink::CONE_EDGE));
        let (track, glow) = (css(ink::TRACK), css(ink::TRACK_GLOW));
        let (head, head_dot) = (css(ink::HEAD), css(ink::HEAD_DOT));
        let (rim, intercept) = (css(ink::HEAD_RIM), css(ink::INTERCEPT));
        let chip_fill = css(theme::LABEL_CHIP);
        for t in &v.tracks {
            // The cone: a box per step, along the middle of its quad, as
            // wide as the quad halfway along.
            for q in &t.cone {
                let [a, b, c, d] = (*q).map(|p| at(p));
                let w = (a.distance(b) + c.distance(d)) * 0.5;
                if let Some(s) = seg((a + b) * 0.5, (c + d) * 0.5, w, &cone, false) {
                    shapes.push((s, String::new()));
                }
            }
            for r in t.cone_edge.iter().flat_map(&runs) {
                line(&r, 1.0, &cone_edge, false, &mut shapes);
            }
            for r in t.behind.iter().flat_map(&runs) {
                line(&r, 5.0, &glow, false, &mut shapes);
                line(&r, 1.6, &track, true, &mut shapes);
            }
            for r in t.ahead.iter().flat_map(&runs) {
                dashed(&r, 1.6, &track, 3.0, 6.0, &mut shapes);
            }
            // A time every half hour: a dot, its time centred over it.
            for (p, label) in &t.times {
                let s = at(*p);
                shapes.push((disc(s, 2.6, &track), String::new()));
                words.push((
                    boxed(
                        s.x - 40.0,
                        s.y - 8.0 - 16.0,
                        80.0,
                        16.0,
                        &format!(
                            "display: flex; justify-content: center; align-items: flex-end; \
                             white-space: nowrap; line-height: 1; font-size: {}px; color: {track}",
                            theme::SIZE_TINY
                        ),
                    ),
                    label.clone(),
                ));
            }
            // Where some were brought down: a cross.
            for p in &t.intercepts {
                let p = at(*p);
                for (a, b) in [
                    (Vec2::new(-4.0, -4.0), Vec2::new(4.0, 4.0)),
                    (Vec2::new(4.0, -4.0), Vec2::new(-4.0, 4.0)),
                ] {
                    if let Some(s) = seg(p + a, p + b, 1.8, &intercept, false) {
                        shapes.push((s, String::new()));
                    }
                }
            }
            // The head: a glow, its members, and its chip.
            let Some(h) = &t.head else { continue };
            let c = at(h.at);
            shapes.push((disc(c, 18.0, &head), String::new()));
            for n in 0..7 {
                let a = n as f32 * 2.4;
                let r = 3.0 + (n % 3) as f32 * 3.2;
                shapes.push((
                    disc(c + Vec2::new(a.cos(), a.sin()) * r, 2.2, &head_dot),
                    String::new(),
                ));
            }
            words.push((
                format!(
                    "position: absolute; left: {:.1}px; top: {:.1}px; padding: {}px {}px; \
                     background: {chip_fill}; border: {}px solid {rim}; border-radius: {}px; \
                     white-space: nowrap; line-height: 1.2; font-size: {}px; color: {track}",
                    c.x + 14.0,
                    c.y + 10.0,
                    CHIP_PAD.1,
                    CHIP_PAD.0,
                    if h.selected { 2 } else { 1 },
                    theme::RADIUS_SM,
                    theme::SIZE_CHIP
                ),
                h.label.clone(),
            ));
        }
        shapes.truncate(MAX_BOXES);
        self.shapes.sync(shapes);
        self.words.sync(words);
    }
}

/// Mounts the marks while a lens is open, restyles them to each frame's,
/// and clears them when it closes. `PostUpdate`, before `DriveUi`, as every
/// library screen.
pub fn drive_lib_scenario_map(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    view: Res<ScenarioView>,
    metrics: Res<UiMetrics>,
) {
    if !lib.scenario {
        return;
    }
    let k = px_scale(metrics.ui_scale());
    let dark = theme::p().ink == theme::DARK.ink;
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        let Some(mode) = view.mode_frame() else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        };
        if m.as_ref().is_some_and(|live| live.dark != dark) {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let live = m.get_or_insert_with(|| {
            let live = Live::new(dark);
            crate::systems::lib_host::mount_keyed(KEY, view_of(&live), dark, theme::touch_layout());
            live
        });
        live.frame_to(mode, metrics.viewport_pts() * k);
        live.marks_to(&view.map, k);
    });
}

/// The marks in their clipping box, the card, and the frame over all.
fn view_of(l: &Live) -> View {
    let marks = {
        let s = l.marks.clone();
        move || s.get()
    };
    let shapes = l.shapes.view(false);
    let words = l.words.view(true);
    let tip_at = {
        let s = l.tip_at.clone();
        move || s.get()
    };
    let tip = {
        let t = l.tip.clone();
        move || tip_body(t.get())
    };
    let frame = {
        let s = l.frame.clone();
        move || s.get()
    };
    let bands: Vec<View> = l
        .bands
        .iter()
        .map(|b| {
            let s = b.style.clone();
            let style = move || s.get();
            let stripes = b.stripes.view(false);
            view! { <div style={style}>{stripes}</div> }
        })
        .collect();
    view! {
        <div style={marks}>
            {shapes}
            {words}
        </div>
        <div style={tip_at}>{tip}</div>
        <div style={frame}></div>
        {bands}
    }
}

/// The card beside the pointer over an area: its name, the figure there and
/// the band that colours it.
fn tip_body(b: Option<TipBody>) -> View {
    let Some(b) = b else {
        return View::Empty;
    };
    let p = theme::p();
    let (cream, dim) = (css(p.cream), css(p.text_dim));
    let name_style = format!(
        "color: {cream}; font-size: {}px; line-height: 1.3",
        theme::SIZE_BODY
    );
    let value_style = format!(
        "color: {cream}; font-size: {}px; font-weight: 700; line-height: 1.25",
        theme::SIZE_HEADING
    );
    let [r, g, bl] = b.colour;
    let swatch = format!(
        "flex: none; width: 10px; height: 10px; border-radius: {}px; background: rgb({r} {g} {bl})",
        theme::RADIUS_SM
    );
    let band_style = format!("color: {dim}; font-size: {}px", theme::SIZE_SUB);
    let (name, value, band) = (b.name, b.value, b.band);
    view! {
        <div style="display: flex; flex-direction: column; gap: 2px">
            <span style={name_style}>{name}</span>
            <span style={value_style}>{value}</span>
            <div style="display: flex; align-items: center; gap: 6px">
                <span style={swatch}></span>
                <span style={band_style}>{band}</span>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line drops points under a pixel or two apart but keeps its ends.
    #[test]
    fn a_line_drops_crowded_points_and_keeps_its_ends() {
        let run = [
            Vec2::new(0.0, 0.0),
            Vec2::new(0.5, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.5, 0.0),
        ];
        assert_eq!(
            thinned(&run),
            vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(10.5, 0.0)
            ]
        );
    }

    /// A line keeps only its parts on screen, split where it leaves.
    #[test]
    fn a_line_off_screen_is_not_drawn() {
        let run = [
            Vec2::new(-500.0, 50.0),
            Vec2::new(-400.0, 50.0),
            Vec2::new(10.0, 50.0),
            Vec2::new(50.0, 50.0),
            Vec2::new(900.0, 50.0),
            Vec2::new(900.0, 900.0),
            Vec2::new(60.0, 60.0),
        ];
        assert_eq!(
            on_screen(&run, Vec2::new(100.0, 100.0), 8.0),
            vec![
                vec![
                    Vec2::new(-400.0, 50.0),
                    Vec2::new(10.0, 50.0),
                    Vec2::new(50.0, 50.0),
                    Vec2::new(900.0, 50.0)
                ],
                vec![Vec2::new(900.0, 900.0), Vec2::new(60.0, 60.0)],
            ]
        );
    }

    /// Dashes are 3 on and 6 off from the start.
    #[test]
    fn a_dashed_line_is_three_on_six_off() {
        let mut out = Vec::new();
        dashed(
            &[Vec2::new(0.0, 0.0), Vec2::new(30.0, 0.0)],
            1.6,
            "red",
            3.0,
            6.0,
            &mut out,
        );
        // 0–3, 9–12, 18–21, 27–30.
        assert_eq!(out.len(), 4);
    }
}
