//! The map's on-screen controls drawn by the component library (`lib_host`),
//! beside egui's (`systems::map_osd`): the thumbstick, the climb or zoom
//! pair, the jump, face and hand discs, the expressions, the zoom rail with
//! its list of scales, the hand's first-use card and the hand's menu.
//!
//! On with `CARTO_UI_LIB=osd` (or `1`), on the web `?ui=osd`. egui's system
//! keeps doing everything but paint (`OsdLib::lib`): it lays every control
//! out, reads every finger (`TouchGestures` — several at once, where the
//! library has one pointer) and every click, keeps the *slots* so a press
//! there is never the world's, and publishes what it laid out
//! (`OsdLib::view`). This draws that, rect for rect, in egui's points (as
//! DOM the page's px are `ui_scale` times larger: [`Pen`]). Nothing here
//! listens but the hand card and the hand menu, so a press goes through to
//! egui's slot beneath — as DOM by `pointer-events: none`.
//!
//! **Built from boxes.** egui painted every mark with its `Painter`; here a
//! stroke is a thin box turned by `transform: rotate`, a disc or a ring a
//! box with `border-radius: 50%`, and an arc a ring with only its top border
//! coloured, turned to where it belongs (`Pen`).
//!
//! **The hand menu.** Opened as egui's was, by a right-click or a long
//! press on the hand disc's slot (`OsdLib::hand_menu`, set by
//! `map_osd::hand_button`, which then makes no popup of its own); drawn
//! here as a popover over the disc, right-aligned to it. Which hand, Start
//! again (only while the tracking is on), what is wrong, and the line on
//! what it uses. A choice closes it, as a tap in egui's menu did; a tap
//! beside it or Escape closes it too, through a transparent veil over the
//! screen that holds the pointer while it is open. The choices are done by
//! `BodyControls::set_hand_side` and `restart_hand`, what egui's menu
//! calls.
//!
//! **What differs.** No press-shrink or hover lift on the discs (the plates
//! do change on hover and press); the halo round the hand's word is a small
//! plate; the globe's meridian is a stadium, not an ellipse; the Milky Way's
//! arms are two quarter arcs each; an end's push ring fills by quarters
//! until a quarter is full, then smoothly; the hand card closes on
//! "Not now" and Escape but not on a tap beside it.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{ButtonInput, Commands, KeyCode, Res, ResMut};
use mithril::prelude::{
    el, signal, view, Button, Event, Handler, IntoView, Signal, Size, Variant, View,
};

use crate::systems::egui_manager::EguiRef;
use crate::systems::lib_settings::LibScreens;
use crate::systems::map_osd::{self, BodyControls, OsdLib};
use crate::systems::theme;
use crate::systems::theme::tokens::hex;
use crate::systems::zoom_rail::Scale;

/// The pieces' names in the library's host.
const STICK: &str = "osd_stick";
const DISCS: &str = "osd_discs";
const FACES: &str = "osd_faces";
const RAIL: &str = "osd_rail";
const CARD: &str = "osd_hand_card";
const MENU: &str = "osd_hand_menu";

/// The width of the hand card, before the screen narrows it (egui's
/// `set_max_width`).
const CARD_WIDTH: f32 = 420.0;

/// The hand menu's width, before the screen narrows it: room for the
/// line on what it uses in three or four lines.
const MENU_WIDTH: f32 = 240.0;

/// What the hand menu's choices asked for; done by the system.
#[derive(Clone, Copy)]
enum HandAct {
    /// Left (`true`) or Right.
    Side(bool),
    Restart,
    /// A tap beside the menu.
    Close,
}

#[derive(Clone, PartialEq)]
struct Menu {
    v: map_osd::HandMenuView,
    screen: (f32, f32),
    k: f32,
}

/// Every stroke on a disc, as egui's `paint_mark`.
const STROKE: f32 = 2.0;

/// What the card's buttons asked for; done by the system.
#[derive(Clone, Copy)]
enum CardAct {
    Accept,
    Decline,
}

#[derive(Clone, PartialEq)]
struct Stick {
    v: map_osd::StickView,
    k: f32,
}

#[derive(Clone, PartialEq)]
struct Discs {
    discs: Vec<map_osd::DiscView>,
    word: Option<map_osd::HandWordView>,
    screen_w: f32,
    k: f32,
}

#[derive(Clone, PartialEq)]
struct Faces {
    v: map_osd::FacesView,
    k: f32,
}

#[derive(Clone, PartialEq)]
struct RailShape {
    f: map_osd::RailFrame,
    k: f32,
}

#[derive(Clone, PartialEq)]
struct Card {
    width: f32,
}

#[derive(Default)]
struct Screens {
    acts: Rc<RefCell<Vec<CardAct>>>,
    stick: Option<Signal<Stick>>,
    discs: Option<Signal<Discs>>,
    faces: Option<Signal<Faces>>,
    rail: Option<(Signal<RailShape>, Signal<map_osd::RailLive>)>,
    card: Option<Signal<Card>>,
    hand_acts: Rc<RefCell<Vec<HandAct>>>,
    menu: Option<Signal<Menu>>,
}

thread_local! {
    static MOUNTED: RefCell<Option<Screens>> = const { RefCell::new(None) };
}

fn dark() -> bool {
    theme::p().ink == theme::DARK.ink
}

/// Mount, update or take down one piece: `want` is what it should show,
/// `None` for nothing (`lib_plans`' rule).
fn keep<T: Clone + PartialEq + 'static>(
    slot: &mut Option<Signal<T>>,
    key: &str,
    want: Option<T>,
    draw: impl FnOnce(Signal<T>) -> View,
) {
    match (want, slot.as_ref()) {
        (None, None) => {}
        (None, Some(_)) => {
            *slot = None;
            crate::systems::lib_host::clear_keyed(key);
        }
        (Some(v), None) => {
            let s = signal(v);
            crate::systems::lib_host::mount_keyed(
                key,
                draw(s.clone()),
                dark(),
                theme::touch_layout(),
            );
            *slot = Some(s);
        }
        (Some(v), Some(s)) => {
            if s.peek(|now| *now != v) {
                s.set(v);
            }
        }
    }
}

/// Mounts, bridges and unmounts the controls. `PostUpdate`, before
/// `DriveUi`, as every library screen — after `map_osd::render_map_osd`
/// (`Update`) has published the frame's view.
#[allow(clippy::needless_pass_by_value)]
pub fn drive_lib_map_osd(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    osd: Option<ResMut<OsdLib>>,
    mut body: BodyControls,
    keys: Res<ButtonInput<KeyCode>>,
    egui: EguiRef,
    mut commands: Commands,
) {
    if !lib.osd {
        return;
    }
    let Some(mut osd) = osd else {
        // Not registered: from here on, and egui's system reads it.
        commands.insert_resource(OsdLib {
            lib: true,
            ..Default::default()
        });
        return;
    };
    if !osd.lib {
        osd.lib = true;
    }
    // The library's px are egui's points on the Bevy backend; as DOM they
    // are the page's (`lib_place_card::Placement`).
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        egui.ui_scale()
    } else {
        1.0
    };
    let view = osd.view.clone();
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        let screens = m.get_or_insert_with(Screens::default);
        // The card's taps of the last frame, through the methods egui's
        // card calls.
        let acts: Vec<CardAct> = std::mem::take(&mut *screens.acts.borrow_mut());
        for a in acts {
            match a {
                CardAct::Accept => body.accept_hand(),
                CardAct::Decline => body.decline_hand(),
            }
        }
        if body.hand_card_open() && keys.just_pressed(KeyCode::Escape) {
            body.decline_hand();
        }
        // The hand menu's: a choice is done as egui's menu does it, and
        // any of them closes it, as a tap in egui's menu did.
        let hand_acts: Vec<HandAct> = std::mem::take(&mut *screens.hand_acts.borrow_mut());
        let mut close = !hand_acts.is_empty();
        for a in hand_acts {
            match a {
                HandAct::Side(left) => body.set_hand_side(left),
                HandAct::Restart => body.restart_hand(),
                HandAct::Close => {}
            }
        }
        // Gone with the hand disc (another mode, a panel over the
        // controls), and on Escape.
        close |= view.hand_menu.is_none() || keys.just_pressed(KeyCode::Escape);
        if close && osd.hand_menu {
            osd.hand_menu = false;
        }
        let menu = view
            .hand_menu
            .clone()
            .filter(|_| osd.hand_menu)
            .map(|v| Menu {
                v,
                screen: (view.screen.x, view.screen.y),
                k,
            });
        let hand_acts = screens.hand_acts.clone();
        keep(&mut screens.menu, MENU, menu, |s| menu_view(s, &hand_acts));

        keep(
            &mut screens.stick,
            STICK,
            view.stick.map(|v| Stick { v, k }),
            stick_view,
        );
        let discs = (!view.discs.is_empty()).then(|| Discs {
            discs: view.discs.clone(),
            word: view.hand_word,
            screen_w: view.screen.x,
            k,
        });
        keep(&mut screens.discs, DISCS, discs, discs_view);
        keep(
            &mut screens.faces,
            FACES,
            view.faces.clone().map(|v| Faces { v, k }),
            faces_view,
        );

        // The rail: its shape and its moving parts as two signals.
        match (view.rail.clone(), screens.rail.as_ref()) {
            (None, None) => {}
            (None, Some(_)) => {
                screens.rail = None;
                crate::systems::lib_host::clear_keyed(RAIL);
            }
            (Some(f), None) => {
                let shape = signal(RailShape { f, k });
                let live = signal(view.rail_live);
                crate::systems::lib_host::mount_keyed(
                    RAIL,
                    rail_view(shape.clone(), live.clone()),
                    dark(),
                    theme::touch_layout(),
                );
                screens.rail = Some((shape, live));
            }
            (Some(f), Some((shape, live))) => {
                let want = RailShape { f, k };
                if shape.peek(|now| *now != want) {
                    shape.set(want);
                }
                if live.peek(|now| *now != view.rail_live) {
                    live.set(view.rail_live);
                }
            }
        }

        let card = body.hand_card_open().then(|| Card {
            width: CARD_WIDTH.min(view.screen.x - 40.0).max(200.0),
        });
        let acts = screens.acts.clone();
        keep(&mut screens.card, CARD, card, |s| card_view(s, &acts));
    });
}

// ─── The pen: egui's strokes as boxes ────────────────────────────────────────

/// Draws in egui points, `k` px each.
#[derive(Clone, Copy)]
struct Pen {
    k: f32,
}

impl Pen {
    fn px(self, v: f32) -> String {
        format!("{:.2}px", v * self.k)
    }

    /// A box at `(x, y)`, `w` × `h` points, in its parent's frame.
    fn boxed(self, x: f32, y: f32, w: f32, h: f32, rest: &str) -> View {
        el("div")
            .attr("style", self.place(x, y, w, h, rest))
            .into_view()
    }

    /// The style of such a box. Width before the radius, which the cascade
    /// reads a `50%` against.
    fn place(self, x: f32, y: f32, w: f32, h: f32, rest: &str) -> String {
        format!(
            "position: absolute; left: {}; top: {}; width: {}; height: {}; \
             box-sizing: border-box; pointer-events: none; {rest}",
            self.px(x),
            self.px(y),
            self.px(w),
            self.px(h)
        )
    }

    /// A stroke from `a` to `b`, `w` wide, its ends rounded and carried
    /// half a width past the points so two strokes meet without a notch.
    fn line(self, a: (f32, f32), b: (f32, f32), w: f32, ink: &str) -> View {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = dx.hypot(dy) + w;
        let deg = dy.atan2(dx).to_degrees();
        let (mx, my) = ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
        self.boxed(
            mx - len * 0.5,
            my - w * 0.5,
            len,
            w,
            &format!(
                "border-radius: {}; background: {ink}; transform: rotate({deg:.1}deg)",
                self.px(w * 0.5)
            ),
        )
    }

    /// A filled disc of radius `r` at `c`.
    fn dot(self, c: (f32, f32), r: f32, ink: &str) -> View {
        self.boxed(
            c.0 - r,
            c.1 - r,
            2.0 * r,
            2.0 * r,
            &format!("border-radius: 50%; background: {ink}"),
        )
    }

    /// A ring of radius `r` at `c`, its stroke `w` centred on the radius as
    /// egui's `circle_stroke`.
    fn ring(self, c: (f32, f32), r: f32, w: f32, ink: &str) -> View {
        let o = r + w * 0.5;
        self.boxed(
            c.0 - o,
            c.1 - o,
            2.0 * o,
            2.0 * o,
            &format!("border: {} solid {ink}; border-radius: 50%", self.px(w)),
        )
    }

    /// A quarter of that ring, centred `deg` clockwise from the top.
    fn arc(self, c: (f32, f32), r: f32, w: f32, ink: &str, deg: f32) -> View {
        let o = r + w * 0.5;
        self.boxed(
            c.0 - o,
            c.1 - o,
            2.0 * o,
            2.0 * o,
            &format!(
                "border: {} solid transparent; border-top-color: {ink}; border-radius: 50%; \
                 transform: rotate({deg:.1}deg)",
                self.px(w)
            ),
        )
    }

    /// Some strokes at an opacity of their own (egui's `gamma_multiply`).
    fn faded(self, alpha: f32, views: Vec<View>) -> View {
        el("div")
            .attr(
                "style",
                format!("position: absolute; left: 0; top: 0; pointer-events: none; opacity: {alpha:.2}"),
            )
            .children(views)
            .into_view()
    }

    /// A one-line tag whose right edge stands `right` points in from its
    /// parent's right edge, centred on `y`.
    fn tag(self, right: f32, y: f32, h: f32, rest: &str, text: &str) -> View {
        el("div")
            .attr(
                "style",
                format!(
                    "position: absolute; right: {}; top: {}; height: {}; box-sizing: border-box; \
                     display: flex; align-items: center; white-space: nowrap; pointer-events: none; {rest}",
                    self.px(right),
                    self.px(y - h * 0.5),
                    self.px(h)
                ),
            )
            .child(text.to_owned())
            .into_view()
    }
}

// ─── The discs and their marks ───────────────────────────────────────────────

/// The engaged ring of a disc.
fn tone(t: map_osd::Tone) -> &'static str {
    match t {
        map_osd::Tone::Ring => "var(--m-accent-hover)",
        map_osd::Tone::Bright => "var(--m-accent-text)",
        map_osd::Tone::Warn => "var(--m-warning)",
    }
}

/// `theme::map_disc`'s three plates: engaged, hovered, at rest — fill, ring
/// and ink.
fn plate(d: &map_osd::DiscState, t: map_osd::Tone) -> (&'static str, &'static str, &'static str) {
    if d.on {
        ("var(--m-accent)", tone(t), "var(--m-on-accent)")
    } else if d.hovered {
        (
            "var(--m-surface-raised)",
            "var(--m-accent)",
            "var(--m-text-strong)",
        )
    } else {
        ("var(--m-surface)", "var(--m-border)", "var(--m-text)")
    }
}

/// A round control where egui laid it out: its plate, and what `inner`
/// draws on it, given its centre (in the disc's own frame) and its ink.
fn disc(
    p: Pen,
    d: &map_osd::DiscState,
    t: map_osd::Tone,
    inner: impl FnOnce((f32, f32), &'static str) -> Vec<View>,
) -> View {
    let size = d.rect.width();
    let c = (size * 0.5, size * 0.5);
    let (fill, ring, ink) = plate(d, t);
    // egui's plate: radius half the disc less a point, a 1.5 stroke on it.
    let o = size * 0.5 - 1.0 + 0.75;
    let mut kids = vec![p.boxed(
        c.0 - o,
        c.1 - o,
        2.0 * o,
        2.0 * o,
        &format!(
            "border-radius: 50%; background: {fill}; border: {} solid {ring}; box-shadow: var(--m-shadow)",
            p.px(1.5)
        ),
    )];
    kids.extend(inner(c, ink));
    el("div")
        .attr("style", p.place(d.rect.min.x, d.rect.min.y, size, size, ""))
        .children(kids)
        .into_view()
}

/// `map_osd::paint_mark`, stroke for stroke.
fn mark(p: Pen, m: map_osd::Mark, c: (f32, f32), s: f32, ink: &str) -> Vec<View> {
    use map_osd::Mark;
    let at = |x: f32, y: f32| (c.0 + x, c.1 + y);
    let w = STROKE;
    match m {
        Mark::Plus => vec![
            p.line(at(-s, 0.0), at(s, 0.0), w, ink),
            p.line(at(0.0, -s), at(0.0, s), w, ink),
        ],
        Mark::Minus => vec![p.line(at(-s, 0.0), at(s, 0.0), w, ink)],
        Mark::Jump => {
            let (dx, dy) = (s, s * 0.55);
            vec![
                p.line(at(-dx, 0.0), at(0.0, -dy * 1.6), w, ink),
                p.line(at(0.0, -dy * 1.6), at(dx, 0.0), w, ink),
                p.line(at(-dx, dy), at(dx, dy), w, ink),
            ]
        }
        Mark::Face => vec![
            p.ring(c, s * 1.1, w, ink),
            p.dot(at(-s * 0.4, -s * 0.3), 1.8, ink),
            p.dot(at(s * 0.4, -s * 0.3), 1.8, ink),
            // The smile: the bottom of a ring.
            p.arc(c, s * 0.6, w, ink, 180.0),
        ],
        Mark::Hand => {
            // A palm and four fingers, the thumb out to the side.
            let (pw, ph) = (s * 1.3, s * 1.0);
            let (left, top) = (c.0 - pw * 0.5, c.1 + s * 0.45 - ph * 0.5);
            let mut v = vec![p.boxed(
                left - w * 0.5,
                top - w * 0.5,
                pw + w,
                ph + w,
                &format!(
                    "border: {} solid {ink}; border-radius: {}",
                    p.px(w),
                    p.px(s * 0.3 + w * 0.5)
                ),
            )];
            for (i, len) in [0.9_f32, 1.1, 1.05, 0.8].iter().enumerate() {
                let x = left + pw * (0.14 + 0.24 * i as f32);
                v.push(p.line((x, top), (x, top - s * len), w, ink));
            }
            v.push(p.line(
                (left, top + ph * 0.5),
                (left - s * 0.5, top - s * 0.1),
                w,
                ink,
            ));
            v
        }
        Mark::Up => {
            let (dx, dy) = (s, s * 0.6);
            vec![
                p.line(at(-dx, dy), at(0.0, -dy), w, ink),
                p.line(at(0.0, -dy), at(dx, dy), w, ink),
            ]
        }
        Mark::Down => {
            let (dx, dy) = (s, s * 0.6);
            vec![
                p.line(at(-dx, -dy), at(0.0, dy), w, ink),
                p.line(at(0.0, dy), at(dx, -dy), w, ink),
            ]
        }
    }
}

/// `map_osd::paint_scale`: the globe a meridian and an equator in a ring;
/// the solar system a Sun in two orbits with a planet; the Milky Way a core
/// with two arms; the universe a web of points.
fn scale_icon(p: Pen, c: (f32, f32), s: f32, scale: Scale, ink: &str) -> Vec<View> {
    let line = (s * 0.16).max(1.3);
    let thin = (s * 0.11).max(1.0);
    let at = |x: f32, y: f32| (c.0 + x * s, c.1 + y * s);
    match scale {
        Scale::Globe => vec![
            p.ring(c, s, line, ink),
            // The meridian: a stadium where egui drew an ellipse.
            p.boxed(
                c.0 - s * 0.45 - line * 0.5,
                c.1 - s - line * 0.5,
                s * 0.9 + line,
                s * 2.0 + line,
                &format!("border: {} solid {ink}; border-radius: 50%", p.px(line)),
            ),
            p.line(at(-1.0, 0.0), at(1.0, 0.0), line, ink),
        ],
        Scale::SolarSystem => vec![
            p.dot(c, s * 0.3, ink),
            p.faded(
                0.75,
                vec![p.ring(c, s * 0.62, thin, ink), p.ring(c, s, thin, ink)],
            ),
            p.dot(at(0.71, -0.71), s * 0.2, ink),
            p.dot(at(-0.44, 0.44), s * 0.15, ink),
        ],
        Scale::MilkyWay => {
            // Each arm a widening spiral over 288°, as two quarter arcs:
            // its inner half at 0.48 of the radius, its outer at 0.83.
            let mut v = vec![p.dot(c, s * 0.22, ink)];
            for turn in [0.0_f32, 180.0] {
                v.push(p.arc(c, s * 0.48, line, ink, turn + 162.0));
                v.push(p.arc(c, s * 0.83, line, ink, turn + 306.0));
            }
            v
        }
        Scale::Universe => {
            let pts = [
                (-0.7, -0.5),
                (0.55, -0.75),
                (0.0, 0.0),
                (-0.75, 0.65),
                (0.7, 0.7),
                (0.95, -0.05),
            ];
            let pt = |(x, y): (f32, f32)| at(x, y);
            let mut strokes: Vec<View> = [0, 1, 3, 4, 5]
                .into_iter()
                .map(|i| p.line(pt(pts[2]), pt(pts[i]), thin, ink))
                .collect();
            strokes.push(p.line(pt(pts[0]), pt(pts[1]), thin, ink));
            let mut v = vec![p.faded(0.75, strokes)];
            for (i, a) in pts.iter().enumerate() {
                v.push(p.dot(pt(*a), s * if i == 2 { 0.22 } else { 0.15 }, ink));
            }
            v
        }
    }
}

// ─── The pieces ──────────────────────────────────────────────────────────────

/// The well and the knob: four nodes restyled as the thumb moves.
fn stick_view(s: Signal<Stick>) -> View {
    let r = map_osd::STICK_RADIUS;
    let kr = map_osd::STICK_KNOB_RADIUS;
    let node = |style: Box<dyn Fn(&Stick) -> String>| -> View {
        let s = s.clone();
        el("div").attr("style", move || style(&s.get())).into_view()
    };
    vec![
        // The well is a guide, not a plate (`map_osd::thumbstick`): the
        // map shows through its fill; the hairline says where travel ends.
        node(Box::new(move |st: &Stick| {
            let (p, c) = (Pen { k: st.k }, st.v.centre);
            p.place(
                c.x - r,
                c.y - r,
                2.0 * r,
                2.0 * r,
                "border-radius: 50%; background: var(--m-surface); opacity: 0.55",
            )
        })),
        node(Box::new(move |st: &Stick| {
            let (p, c) = (Pen { k: st.k }, st.v.centre);
            let o = r + 0.75;
            p.place(
                c.x - o,
                c.y - o,
                2.0 * o,
                2.0 * o,
                &format!(
                    "border: {} solid var(--m-border); border-radius: 50%",
                    p.px(1.5)
                ),
            )
        })),
        node(Box::new(move |st: &Stick| {
            let (p, c) = (Pen { k: st.k }, st.v.knob);
            let (fill, ring) = if st.v.engaged {
                ("var(--m-accent-soft)", "var(--m-accent-hover)")
            } else {
                ("var(--m-surface)", "var(--m-border)")
            };
            let o = kr + 0.75;
            p.place(
                c.x - o,
                c.y - o,
                2.0 * o,
                2.0 * o,
                &format!(
                    "border-radius: 50%; background: {fill}; border: {} solid {ring}",
                    p.px(1.5)
                ),
            )
        })),
    ]
    .into_view()
}

/// The round controls and the hand's word.
fn discs_view(s: Signal<Discs>) -> View {
    (move || -> View {
        let d = s.get();
        let p = Pen { k: d.k };
        let mut v: Vec<View> = d
            .discs
            .iter()
            .map(|dv| {
                let m = dv.mark;
                let size = dv.disc.rect.width();
                disc(p, &dv.disc, dv.tone, |c, ink| {
                    mark(p, m, c, size * 0.22, ink)
                })
            })
            .collect();
        if let Some(w) = d.word {
            // egui's halo text, as a small plate under the word.
            v.push(p.tag(
                d.screen_w - w.right,
                w.y,
                theme::SIZE_TINY + 6.0,
                &format!(
                    "padding: 0 {}; border-radius: var(--m-radius-full); background: var(--m-surface); \
                     color: {}; font-size: {}",
                    p.px(5.0),
                    tone(w.tone),
                    p.px(theme::SIZE_TINY)
                ),
                w.text,
            ));
        }
        v.into_view()
    })
    .into_view()
}

/// The expressions: a frame and a column of buttons, where egui's are.
fn faces_view(s: Signal<Faces>) -> View {
    (move || -> View {
        let f = s.get();
        let p = Pen { k: f.k };
        let r = f.v.frame;
        let mut v = vec![p.boxed(
            r.min.x,
            r.min.y,
            r.width(),
            r.height(),
            "background: var(--m-surface); border: 1px solid var(--m-border); \
             border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow)",
        )];
        for (row, label) in &f.v.rows {
            v.push(
                el("div")
                    .attr(
                        "style",
                        p.place(
                            row.min.x,
                            row.min.y,
                            row.width(),
                            row.height(),
                            &format!(
                                "display: flex; align-items: center; justify-content: center; \
                                 background: var(--m-surface-raised); border: 1px solid var(--m-border-strong); \
                                 border-radius: var(--m-radius); color: var(--m-text); font-size: {}",
                                p.px(theme::SIZE_BODY)
                            ),
                        ),
                    )
                    .child(*label)
                    .into_view(),
            );
        }
        v.into_view()
    })
    .into_view()
}

/// The lilac of what lies past the street (`theme::PUBLIC`).
fn public() -> String {
    hex(theme::PUBLIC)
}

/// The zoom rail: a column at egui's, its shape redrawn when the scale or a
/// disc's state changes, its thumb, the jump's ring, the step's name and the
/// push rings when they move; and the list of scales beside it.
fn rail_view(shape: Signal<RailShape>, live: Signal<map_osd::RailLive>) -> View {
    let column = {
        let (shape, live) = (shape.clone(), live.clone());
        move || {
            let sh = shape.get();
            let c = sh.f.column;
            Pen { k: sh.k }.place(
                c.min.x,
                c.min.y,
                c.width(),
                c.height(),
                &format!("opacity: {:.2}", live.get().opacity),
            )
        }
    };
    let parts = {
        let shape = shape.clone();
        move || -> View { rail_parts(&shape.get()) }
    };
    let moving = {
        let shape = shape.clone();
        move || -> View { rail_moving(&shape.get(), &live.get()) }
    };
    let list = move || -> View { rail_list(&shape.get()) };
    vec![
        el("div")
            .attr("style", column)
            .child(parts)
            .child(moving)
            .into_view(),
        list.into_view(),
    ]
    .into_view()
}

/// The rail's discs and track, in the column's frame.
fn rail_parts(sh: &RailShape) -> View {
    let (f, p) = (&sh.f, Pen { k: sh.k });
    let o = f.column.min;
    let local = |d: &map_osd::DiscState| map_osd::DiscState {
        rect: d.rect.translate(-o.to_vec2()),
        ..*d
    };
    let mut v = Vec::new();

    // The scale disc: the scale's icon, lilac past the Earth.
    let sd = local(&f.scale_disc);
    let size = sd.rect.width();
    let scale = f.scale;
    let lilac = !(sd.on || sd.hovered || scale == Scale::Globe);
    v.push(disc(p, &sd, map_osd::Tone::Ring, |c, ink| {
        let ink = if lilac { public() } else { ink.to_owned() };
        scale_icon(p, c, size * 0.27, scale, &ink)
    }));

    // − and +, or the next scale at an end, with its name beside it.
    for (i, (d, next)) in f.ends.iter().enumerate() {
        let d = local(d);
        let size = d.rect.width();
        let r = size * 0.5 - 1.0;
        let next = *next;
        v.push(disc(p, &d, map_osd::Tone::Ring, |c, ink| match next {
            Some(to) => {
                let mut k = scale_icon(p, c, size * 0.27, to, ink);
                k.push(p.ring(c, r + 1.5, 1.5, "var(--m-accent)"));
                k
            }
            None => {
                let m = if i == 0 {
                    map_osd::Mark::Minus
                } else {
                    map_osd::Mark::Plus
                };
                mark(p, m, c, size * 0.2, ink)
            }
        }));
        if let Some(to) = next {
            let text = format!("{} {}", if i == 0 { "Out to" } else { "In to" }, to.label());
            let cx = d.rect.center();
            v.push(p.tag(
                f.column.width() - (cx.x - r - 10.0),
                cx.y,
                theme::SIZE_SUB + 8.0,
                &format!(
                    "padding: 0 {}; background: var(--m-surface); border: 1px solid var(--m-accent); \
                     border-radius: var(--m-radius); color: var(--m-text-strong); font-size: {}",
                    p.px(7.0),
                    p.px(theme::SIZE_SUB)
                ),
                &text,
            ));
        }
    }

    // The track, lilac past the Earth, plain on the globe; a faint tick
    // between steps.
    let t = f.track.translate(-o.to_vec2());
    let x = t.center().x;
    v.push(p.line((x, t.top()), (x, t.bottom()), 4.0, "var(--m-surface)"));
    let (tone, alpha) = if scale == Scale::Globe {
        ("var(--m-text-dim)".to_owned(), 1.0)
    } else {
        (public(), 0.8)
    };
    let mut ticks = vec![p.line((x, t.top()), (x, t.bottom()), 1.5, &tone)];
    let n = f.steps.max(1);
    let mut faint = Vec::new();
    for k in 1..n {
        let y = t.top() + t.height() * k as f32 / n as f32;
        faint.push(p.line((x - 3.0, y), (x + 3.0, y), 1.0, &tone));
    }
    ticks.push(p.faded(0.6, faint));
    v.push(p.faded(alpha, ticks));
    v.into_view()
}

/// The thumb, the jump's goal, the step's name and the push rings, in the
/// column's frame.
fn rail_moving(sh: &RailShape, l: &map_osd::RailLive) -> View {
    let (f, p) = (&sh.f, Pen { k: sh.k });
    let o = f.column.min;
    let x = f.track.center().x - o.x;
    let mut v = Vec::new();
    if let Some(y) = l.ring_y {
        v.push(p.ring((x, y - o.y), 8.0, 2.0, "var(--m-text-strong)"));
    }
    let ty = l.thumb_y - o.y;
    v.push(p.dot((x, ty), 7.0, "var(--m-text-strong)"));
    v.push(p.ring((x, ty), 9.5, 3.0, "var(--m-border)"));
    if let Some((label, shown)) = l.name {
        v.push(p.faded(
            shown,
            vec![p.tag(
                f.column.width() - (x - 16.0),
                ty,
                theme::SIZE_SUB + 8.0,
                &format!(
                    "padding: 0 {}; background: var(--m-surface); border-radius: var(--m-radius); \
                     color: var(--m-text-strong); font-size: {}",
                    p.px(7.0),
                    p.px(theme::SIZE_SUB)
                ),
                label,
            )],
        ));
    }
    // A push against an end fills the ring round its disc, from the top
    // round clockwise.
    for (i, (d, next)) in f.ends.iter().enumerate() {
        let done = l.push[i].clamp(0.0, 1.0);
        if next.is_none() || done <= 0.0 {
            continue;
        }
        let c = d.rect.center() - o.to_vec2();
        let r = d.rect.width() * 0.5 - 1.0 + 3.5;
        let whole = (done * 4.0).floor() as usize;
        for q in 0..whole {
            v.push(p.arc(
                (c.x, c.y),
                r,
                2.0,
                "var(--m-text-strong)",
                45.0 + 90.0 * q as f32,
            ));
        }
        let end = if done >= 0.25 {
            // The last quarter ends where the push is; it overlaps the
            // whole ones before it.
            done * 360.0 - 45.0
        } else {
            45.0
        };
        if done * 4.0 > whole as f32 {
            v.push(p.arc((c.x, c.y), r, 2.0, "var(--m-text-strong)", end));
        }
    }
    v.into_view()
}

/// The four scales, opened from the scale disc: outermost on top.
fn rail_list(sh: &RailShape) -> View {
    let Some(rows) = &sh.f.list else {
        return View::Empty;
    };
    let p = Pen { k: sh.k };
    let mut v = Vec::new();
    for row in rows {
        let r = row.rect;
        let h = r.height();
        let fill = if row.hovered {
            "var(--m-surface-raised)"
        } else {
            "var(--m-surface)"
        };
        let c = (r.width() - h * 0.5, h * 0.5);
        let pr = h * 0.5 - 3.0;
        let (plate, ink) = if row.on {
            ("var(--m-accent)", "var(--m-on-accent)".to_owned())
        } else if row.scale == Scale::Globe {
            ("var(--m-surface-raised)", "var(--m-text-strong)".to_owned())
        } else {
            ("var(--m-surface-raised)", public())
        };
        let mut kids = vec![p.dot(c, pr, plate)];
        kids.extend(scale_icon(p, c, pr * 0.6, row.scale, &ink));
        kids.push(p.tag(
            h + 10.0 - 3.0,
            c.1,
            h,
            &format!(
                "color: {}; font-size: {}",
                if row.on {
                    "var(--m-text-strong)"
                } else {
                    "var(--m-text)"
                },
                p.px(theme::SIZE_BODY)
            ),
            row.scale.label(),
        ));
        v.push(
            el("div")
                .attr(
                    "style",
                    p.place(
                        r.min.x,
                        r.min.y,
                        r.width(),
                        h,
                        &format!(
                            "background: {fill}; border: 1px solid var(--m-border); \
                             border-radius: var(--m-radius-full)"
                        ),
                    ),
                )
                .children(kids)
                .into_view(),
        );
    }
    v.into_view()
}

/// The hand button's first tap (`map_osd::hand_card`): what it uses and who
/// feels it, said once. The veil listens, so no tap reaches the map or a
/// control beneath; the controls step aside meanwhile.
fn card_view(s: Signal<Card>, acts: &Rc<RefCell<Vec<CardAct>>>) -> View {
    let on = |a: CardAct| -> Option<Handler> {
        let acts = acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    };
    let column = move || {
        format!(
            "flex: none; width: {:.0}px; display: flex; flex-direction: column; gap: 8px; \
             padding: 20px; background: var(--m-surface); border: 1px solid var(--m-border); \
             border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow)",
            s.get().width
        )
    };
    let lines: Vec<View> = map_osd::HAND_CARD_LINES
        .iter()
        .map(|line| view! { <p style="margin: 0; color: var(--m-text)">{*line}</p> })
        .collect();
    let (decline, accept) = (on(CardAct::Decline), on(CardAct::Accept));
    view! {
        <div style="position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; align-items: center; justify-content: center; padding: 20px; background: var(--m-backdrop)" on:click={|_: &Event| {}}>
            <div style={column}>
                <h2 style="margin: 0 0 4px; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{map_osd::HAND_CARD_TITLE}</h2>
                {lines}
                <div style="display: flex; gap: 8px; margin-top: 8px">
                    <Button variant=Variant::Secondary onclick={decline}>{map_osd::HAND_CARD_DECLINE}</Button>
                    <Button variant=Variant::Primary onclick={accept}>{map_osd::HAND_CARD_ACCEPT}</Button>
                </div>
            </div>
        </div>
    }
}

/// The hand disc's menu (`map_osd::hand_button`'s context menu): a popover
/// right-aligned to the disc and standing over it, and a transparent veil
/// over the rest of the screen that closes it on a tap. The popover comes
/// after the veil rather than inside it, so a tap on its padding does not
/// reach the veil.
fn menu_view(s: Signal<Menu>, acts: &Rc<RefCell<Vec<HandAct>>>) -> View {
    let acts = acts.clone();
    (move || -> View {
        let m = s.get();
        let p = Pen { k: m.k };
        let on = |a: HandAct| -> Option<Handler> {
            let acts = acts.clone();
            Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
        };
        let (sw, sh) = m.screen;
        let width = MENU_WIDTH.min(sw - 2.0 * theme::OSD_MARGIN).max(0.0);
        // Right-aligned to the disc, kept on the screen.
        let right = (sw - m.v.disc.max.x)
            .min(sw - width - theme::OSD_MARGIN)
            .max(0.0);
        let bottom = sh - m.v.disc.min.y + theme::OSD_GAP;
        let pop = format!(
            "position: absolute; right: {}; bottom: {}; width: {}; box-sizing: border-box; \
             display: flex; flex-direction: column; gap: {}; padding: {}; \
             background: var(--m-surface); border: 1px solid var(--m-border); \
             border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
            p.px(right),
            p.px(bottom),
            p.px(width),
            p.px(8.0),
            p.px(12.0)
        );
        let title = format!(
            "font-size: {}; font-weight: 600; color: var(--m-text-strong)",
            p.px(theme::SIZE_BODY)
        );
        let (left_v, right_v) = if m.v.left {
            (Variant::Primary, Variant::Secondary)
        } else {
            (Variant::Secondary, Variant::Primary)
        };
        let problem: View = match m.v.problem.clone() {
            Some(text) => {
                let style = format!(
                    "margin: 0; color: var(--m-warning); font-size: {}",
                    p.px(theme::SIZE_SUB)
                );
                view! { <p style={style}>{text}</p> }
            }
            None => View::Empty,
        };
        let note = format!(
            "margin: 0; color: var(--m-text-dim); font-size: {}",
            p.px(theme::SIZE_CHIP)
        );
        let veil = el("div")
            .attr(
                "style",
                "position: absolute; left: 0; top: 0; right: 0; bottom: 0".to_owned(),
            )
            .on("click", {
                let acts = acts.clone();
                move |_: &Event| acts.borrow_mut().push(HandAct::Close)
            })
            .into_view();
        let can_restart = m.v.can_restart;
        let card = view! {
            <div style={pop} on:click={|_: &Event| {}}>
                <span style={title}>{map_osd::HAND_MENU_TITLE}</span>
                <div style="display: flex; gap: 8px">
                    <Button size=Size::Sm variant={left_v} onclick={on(HandAct::Side(true))}>"Left"</Button>
                    <Button size=Size::Sm variant={right_v} onclick={on(HandAct::Side(false))}>"Right"</Button>
                </div>
                <div style="display: flex">
                    <Button size=Size::Sm variant=Variant::Secondary disabled={!can_restart} onclick={on(HandAct::Restart)}>{map_osd::HAND_MENU_RESTART}</Button>
                </div>
                {problem}
                <p style={note}>{map_osd::HAND_MENU_NOTE}</p>
            </div>
        };
        vec![veil, card].into_view()
    })
    .into_view()
}
