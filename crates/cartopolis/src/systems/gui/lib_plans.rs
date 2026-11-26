//! The Plans layer drawn by the component library (`lib_host`): its marks
//! over the map, the chip at the top while the layer is on, the card a dot,
//! a square or the ring's *Plans here* opens, and the chip that says a
//! followed place has news.
//!
//! **The marks** — a notice's dot, a council item's and a work's square,
//! and under them the outlines of notice areas and works — are placed by
//! `plans::place_plans` (`PlansView::marks`, `PlansView::strokes`) and drawn
//! here through `lib_pins::Anchored`, the keyed list everything anchored to
//! the world goes through. A dot is a bordered disc, a square a bordered
//! box with its glyph; an outline is a run of thin boxes, each turned onto
//! its piece of line (a dash, or a whole segment of a solid line), and a
//! tree's ring a bordered circle. Nothing in an outline listens; a tap on a
//! mark goes back as `Plans::lib_tapped` and opens its card there. The
//! outlines stand under the place names (-3), the marks over them and under
//! the pins (-1), as egui stacked them. The library has no hover, so a mark
//! does not grow under the pointer; its words are the node's `title`.
//!
//! The card's and the news chip's reasons to stand aside are worked out by
//! `plans::drive_plans_card` and `plans::drive_plans_news`
//! (`Plans::lib_view`). What a card shows is one model
//! (`Plans::card_model`, a list of `plans::Block`s), and a tap goes through
//! `Plans::act` and `Plans::open_news`.
//!
//! **Placement.** The card stands where the place card does, centred over
//! the dock (`lib_place_card::Placement`); the two chips are centred under
//! the top bar, the news one under the layer's chip when that is up. A chip
//! is a full-width row that lets the pointer through, holding a pill that
//! does not.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::layer_state::DataLayers;
use crate::systems::lib_pins::{boxed, css, px_scale, Anchored, Spot};
use crate::systems::lib_place_card::{icon, Placement, CARD_WIDTH};
use crate::systems::lib_settings::LibScreens;
use crate::systems::plans::{
    chip_words, follow_explained, follow_tip, Block, CardAct, CardModel, Ink, MarkLook, Marker,
    PlanStroke, Plans, StrokeInk, StrokeLook, TextSize, COUNCIL_R, DOT_R, OPEN_R, STROKE_W, TREE_R,
    WORK, WORK_R,
};
use crate::systems::theme::tokens::hex;
use crate::systems::ui_input::UiMetrics;
use crate::systems::user_store::UserStore;
use crate::systems::{icons, theme};

/// The three pieces' names in the library's host.
const CHIP: &str = "plans_chip";
const CARD: &str = "plans_card";
const NEWS: &str = "plans_news";
/// The marks over the map, and their outlines.
const MARKS: &str = "plans_marks";

/// The stacking of the marks' two lists, as egui's: the outlines under the
/// place names (`lib_place_names`, -3), the marks over them and under the
/// pins (`lib_pins`, -1).
const STROKES_Z: i32 = -4;
const MARKS_Z: i32 = -2;
/// A piece of outline's length is kept in quarter px and its turn in tenths
/// of a degree: finer than shows, coarse enough that a still map restyles
/// nothing.
const LEN_STEPS: f32 = 4.0;
const TURN_STEPS: f32 = 10.0;

/// Points from the top of the map chrome to a chip's top, as egui's.
const CHIP_TOP_PTS: f32 = 52.0;
/// The layer's chip as tall as it is drawn, with room under it.
const CHIP_STEP_PTS: f32 = 48.0;

/// What a tap asked for; done by the system.
#[derive(Clone)]
enum Act {
    Close,
    Card(CardAct),
    Url(String),
    LayerOff,
    News,
}

/// The layer's chip as drawn.
#[derive(Clone, PartialEq)]
struct ChipState {
    top: f32,
    what: String,
    open: Option<String>,
    works: Option<String>,
}

/// The card as drawn.
#[derive(Clone, PartialEq)]
struct CardState {
    place: Placement,
    model: CardModel,
}

/// The news chip as drawn.
#[derive(Clone, PartialEq)]
struct NewsState {
    top: f32,
    width: f32,
    head: String,
    line: String,
}

/// A mark as drawn: everything but where it is.
#[derive(Clone, PartialEq)]
struct MarkStyle {
    title: String,
    look: MarkLook,
}

/// A piece of outline as drawn: a line's length and turn (in [`LEN_STEPS`]
/// and [`TURN_STEPS`]) and colour, or a tree's ring.
#[derive(Clone, PartialEq)]
enum Piece {
    Line { len: i32, turn: i32, ink: StrokeInk },
    Ring,
}

/// The marks' mount: its two lists, and the key of the mark tapped since
/// the last frame.
struct Marks {
    marks: Anchored<MarkStyle>,
    pieces: Anchored<Piece>,
    tapped: Rc<RefCell<Option<String>>>,
}

struct Screens {
    acts: Rc<RefCell<Vec<Act>>>,
    chip: Option<Signal<ChipState>>,
    card: Option<Signal<CardState>>,
    news: Option<Signal<NewsState>>,
    marks: Marks,
}

thread_local! {
    static MOUNTED: RefCell<Option<Screens>> = const { RefCell::new(None) };
}

fn on(acts: &Rc<RefCell<Vec<Act>>>, a: Act) -> Handler {
    let acts = acts.clone();
    Rc::new(move |_: &Event| acts.borrow_mut().push(a.clone())) as Handler
}

/// Mount, update or take down one piece: `want` is what it should show,
/// `None` for nothing.
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
            let p = theme::p();
            crate::systems::lib_host::mount_keyed(
                key,
                draw(s.clone()),
                p.ink == theme::DARK.ink,
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

/// Mounts the marks once and mirrors `place_plans`' frame of them; mounts,
/// bridges and unmounts the chip, the card and the news chip. `PostUpdate`,
/// before `DriveUi`, as every library screen.
pub fn drive_lib_plans(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut plans: ResMut<Plans>,
    mut dl: ResMut<DataLayers>,
    mut store: ResMut<UserStore>,
    metrics: Res<UiMetrics>,
) {
    if !lib.plans {
        return;
    }
    let size = metrics.viewport_pts();
    let ui_scale = metrics.ui_scale();
    // The library's px are egui's points on the Bevy backend; as DOM they are
    // the page's (`Placement::over_the_dock` says why).
    let k = px_scale(ui_scale);
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        let screens = m.get_or_insert_with(|| {
            let marks = Marks {
                marks: Anchored::new(),
                pieces: Anchored::new(),
                tapped: Rc::new(RefCell::new(None)),
            };
            // Mounted once and kept: the marks' colours are the layer's
            // own, the same in both themes, and empty lists draw nothing.
            crate::systems::lib_host::mount_keyed(
                MARKS,
                marks_view(&marks),
                theme::p().ink == theme::DARK.ink,
                theme::touch_layout(),
            );
            Screens {
                acts: Rc::new(RefCell::new(Vec::new())),
                chip: None,
                card: None,
                news: None,
                marks,
            }
        });
        // The marks: last frame's tap back to `place_plans`, this frame's
        // marks and outlines in.
        if let Some(key) = screens.marks.tapped.borrow_mut().take() {
            plans.lib_tapped = Some(key);
        }
        screens
            .marks
            .pieces
            .sync(plans.lib_view.strokes.iter().map(|s| piece(s, k)));
        screens
            .marks
            .marks
            .sync(plans.lib_view.marks.iter().map(|m| {
                (
                    m.key.clone(),
                    Spot::at(m.at.x * k, m.at.y * k),
                    MarkStyle {
                        title: m.title.clone(),
                        look: m.look,
                    },
                )
            }));
        // The taps of the frame, before anything else moves.
        let acts: Vec<Act> = std::mem::take(&mut *screens.acts.borrow_mut());
        for a in acts {
            match a {
                Act::Close => plans.card = None,
                Act::Card(c) => plans.act(c, &mut store),
                Act::Url(url) => crate::systems::data_layers::open_url(&url),
                Act::LayerOff => dl.plans_enabled = false,
                Act::News => plans.open_news(&mut store),
            }
        }
        let top = (crate::systems::dock::reserved_top() + theme::OSD_MARGIN + CHIP_TOP_PTS) * k;

        let chip = plans.lib_view.chip.map(|(drawn, open, works, high)| {
            let (what, open, works) = chip_words((drawn, open, works), high);
            ChipState {
                top,
                what,
                open,
                works,
            }
        });
        let acts = screens.acts.clone();
        keep(&mut screens.chip, CHIP, chip, |s| chip_view(s, &acts));

        let card = plans
            .card
            .filter(|_| !plans.lib_view.card_hidden)
            .map(|card| CardState {
                place: Placement::over_the_dock(size.x, size.y, ui_scale),
                model: plans.card_model(card, &dl, &store.data.plans_follow),
            });
        keep(&mut screens.card, CARD, card, |s| card_view(s, &acts));

        let news = if plans.lib_view.news_hidden {
            None
        } else {
            plans.news_lines().map(|(head, line)| NewsState {
                top: top
                    + if dl.plans_enabled {
                        CHIP_STEP_PTS * k
                    } else {
                        0.0
                    },
                width: CARD_WIDTH.min(size.x - 2.0 * theme::OSD_MARGIN).max(0.0) * k,
                head,
                line,
            })
        };
        keep(&mut screens.news, NEWS, news, |s| news_view(s, &acts));
    });
}

/// One piece of outline for the list: keyed, placed at its middle (a line)
/// or its centre (a ring), in the library's px.
fn piece(s: &PlanStroke, k: f32) -> (String, Spot, Piece) {
    match s.look {
        StrokeLook::Line { a, b, ink } => {
            let (a, b) = (a * k, b * k);
            let (d, mid) = (b - a, (a + b) / 2.0);
            (
                s.key.clone(),
                Spot::at(mid.x, mid.y),
                Piece::Line {
                    len: (d.length() * LEN_STEPS).round() as i32,
                    turn: (d.y.atan2(d.x).to_degrees() * TURN_STEPS).round() as i32,
                    ink,
                },
            )
        }
        StrokeLook::Ring { at } => (s.key.clone(), Spot::at(at.x * k, at.y * k), Piece::Ring),
    }
}

/// The marks' mount: the outlines' list under the marks' list.
fn marks_view(m: &Marks) -> View {
    let pieces = m.pieces.view(STROKES_Z, |_, p| piece_view(p));
    let marks = {
        let tapped = m.tapped.clone();
        m.marks
            .view(MARKS_Z, move |key, s| mark_view(key, s, &tapped))
    };
    view! {
        {pieces}
        {marks}
    }
}

/// A piece of outline about its item's origin: a thin box as long as the
/// piece, turned onto it, or a tree's ring. Nothing in it listens.
fn piece_view(p: &Piece) -> View {
    let style = match p {
        Piece::Line { len, turn, ink } => {
            let len = *len as f32 / LEN_STEPS;
            let deg = *turn as f32 / TURN_STEPS;
            boxed(
                -len / 2.0,
                -STROKE_W / 2.0,
                len,
                STROKE_W,
                &format!(
                    "background: {}; pointer-events: none; transform: rotate({deg:.1}deg)",
                    css(ink.colour())
                ),
            )
        }
        Piece::Ring => {
            // The stroke centred on the ring's radius, as egui's.
            let o = TREE_R + STROKE_W / 2.0;
            boxed(
                -o,
                -o,
                2.0 * o,
                2.0 * o,
                &format!(
                    "box-sizing: border-box; border-radius: {o:.1}px; border: {STROKE_W:.1}px solid {}; pointer-events: none",
                    css(WORK)
                ),
            )
        }
    };
    view! { <div style={style}></div> }
}

/// A mark about its item's origin: the tappable square it answers in, and
/// the dot or square drawn in its middle.
fn mark_view(key: &str, m: &MarkStyle, tapped: &Rc<RefCell<Option<String>>>) -> View {
    let tap: Handler = {
        let (tapped, key) = (tapped.clone(), key.to_string());
        Rc::new(move |_: &Event| *tapped.borrow_mut() = Some(key.clone()))
    };
    let (cream, ink) = (css(theme::DARK.cream), css(theme::DARK.ink));
    let (side, inner) = match m.look {
        MarkLook::Notice { open } => {
            // Orange on cream while its response period runs, grey on ink after.
            let (r, fill, rim) = if open {
                (OPEN_R, css(theme::BRAND), cream)
            } else {
                (DOT_R, css(theme::DARK.accent), ink)
            };
            let side = 2.0 * r + 8.0;
            let o = r + STROKE_W / 2.0;
            let disc = boxed(
                side / 2.0 - o,
                side / 2.0 - o,
                2.0 * o,
                2.0 * o,
                &format!(
                    "box-sizing: border-box; border-radius: {o:.1}px; background: {fill}; border: {STROKE_W:.1}px solid {rim}"
                ),
            );
            (side, view! { <div style={disc}></div> })
        }
        // A tick for a decision taken, a dot for business still open.
        MarkLook::Council { decided } => square(
            COUNCIL_R,
            css(theme::COUNCIL),
            cream.clone(),
            cream,
            if decided { icons::CHECK } else { icons::DOT },
        ),
        MarkLook::Work { glyph } => square(WORK_R, css(WORK), ink.clone(), ink, glyph),
    };
    let frame = boxed(-side / 2.0, -side / 2.0, side, side, "cursor: pointer");
    let title = m.title.clone();
    view! {
        <div style={frame} title={title} on:click={move |e: &Event| tap(e)}>{inner}</div>
    }
}

/// A council item's or a work's square of half-size `r`, its rim and its
/// glyph in `ink`, centred in the square it answers taps in; that square's
/// side, and the view.
fn square(r: f32, fill: String, rim: String, ink: String, glyph: &'static str) -> (f32, View) {
    let side = 2.0 * r + 6.0;
    let o = r + STROKE_W / 2.0;
    let style = boxed(
        side / 2.0 - o,
        side / 2.0 - o,
        2.0 * o,
        2.0 * o,
        &format!(
            "box-sizing: border-box; display: flex; align-items: center; justify-content: center; \
             border-radius: 6px; background: {fill}; border: {STROKE_W:.1}px solid {rim}; \
             color: {ink}; font-family: 'Material Symbols Rounded'; font-size: {:.1}px; line-height: 1",
            r * 1.3
        ),
    );
    (side, view! { <div style={style}>{glyph}</div> })
}

/// A full-width row at `top` that lets the pointer through, centring what
/// it holds.
fn centred_at(top: f32) -> String {
    format!(
        "position: absolute; left: 0; right: 0; top: {top:.0}px; display: flex; \
         justify-content: center; padding: 0 12px; pointer-events: none"
    )
}

/// A small swatch, in a colour of the layer's own (a content palette, as
/// the dots on the map are).
fn swatch(colour: theme::Rgba8, round: bool) -> View {
    let radius = if round { "var(--m-radius-full)" } else { "3px" };
    let style = format!(
        "flex: none; width: 10px; height: 10px; border-radius: {radius}; background: {}",
        hex(colour)
    );
    view! { <span style={style}></span> }
}

/// The layer's chip: what is drawn, how many still open, the works, and ✕.
fn chip_view(s: Signal<ChipState>, acts: &Rc<RefCell<Vec<Act>>>) -> View {
    let wrap = {
        let s = s.clone();
        move || centred_at(s.get().top)
    };
    let body = {
        let s = s.clone();
        move || -> View {
            let c = s.get();
            let open: View = match c.open {
                Some(t) => view! {
                    <span style="display: flex; align-items: center; gap: 6px; white-space: nowrap">{swatch(theme::BRAND, true)}{t}</span>
                },
                None => View::Empty,
            };
            let works: View = match c.works {
                Some(t) => view! {
                    <span style="display: flex; align-items: center; gap: 6px; white-space: nowrap">{swatch(WORK.into(), false)}{t}</span>
                },
                None => View::Empty,
            };
            view! {
                <span style="white-space: nowrap; color: var(--m-text-dim)">{c.what}</span>
                {open}
                {works}
            }
        }
    };
    let off = on(acts, Act::LayerOff);
    view! {
        <div style={wrap}>
            <div on:click={|_: &Event| {}} style="pointer-events: auto; display: flex; align-items: center; flex-wrap: wrap; column-gap: 12px; row-gap: 4px; padding: 4px 6px 4px 16px; background: var(--m-surface); border: 1px solid var(--m-border); border-radius: var(--m-radius-full); box-shadow: var(--m-shadow); font-size: var(--m-font-size-sm); color: var(--m-text)">
                <span style="display: flex; align-items: center; gap: 8px; color: var(--m-text-strong); font-weight: 600">{icon(icons::FLAG)}"Plans"</span>
                {body}
                <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::CLEAR)} icon_only=true onclick={Some(off)} title="Turn the Plans layer off" />
            </div>
        </div>
    }
}

/// The news chip: how many and where, the newest one; a tap opens its card.
fn news_view(s: Signal<NewsState>, acts: &Rc<RefCell<Vec<Act>>>) -> View {
    let wrap = {
        let s = s.clone();
        move || centred_at(s.get().top)
    };
    let pill = {
        let s = s.clone();
        move || {
            format!(
                "pointer-events: auto; width: {:.0}px; display: flex; align-items: center; gap: 12px; \
                 padding: 12px 16px; background: var(--m-surface); border: 1px solid var(--m-border); \
                 border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow); cursor: pointer",
                s.get().width
            )
        }
    };
    let words = {
        let s = s.clone();
        move || -> View {
            let n = s.get();
            view! {
                <span style="color: var(--m-text-strong); font-size: var(--m-font-size-sm); font-weight: 600">{n.head}</span>
                <span style="white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: var(--m-text-dim); font-size: 12px">{n.line}</span>
            }
        }
    };
    let flag = format!("flex: none; color: {}", hex(theme::BRAND));
    let open = on(acts, Act::News);
    view! {
        <div style={wrap}>
            <div style={pill} title="New near a place you follow" on:click={move |e: &Event| open(e)}>
                <span style={flag}>{icon(icons::FLAG)}</span>
                <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">{words}</div>
            </div>
        </div>
    }
}

/// A line's colour as CSS: the theme's text colours, the layer's own for
/// what is still open, the council and the works.
fn ink(i: Ink) -> String {
    match i {
        Ink::Strong => "var(--m-text-strong)".to_string(),
        Ink::Text => "var(--m-text)".to_string(),
        Ink::Dim => "var(--m-text-dim)".to_string(),
        Ink::Open => hex(theme::BRAND),
        Ink::Council => hex(theme::COUNCIL_LIGHT),
        Ink::Work => hex(WORK.into()),
    }
}

fn size(s: TextSize) -> &'static str {
    match s {
        TextSize::Sub => "var(--m-font-size-sm)",
        TextSize::Tiny => "12px",
    }
}

/// A row's marker, as the map draws its dot or square.
fn marker(m: Marker) -> View {
    swatch(
        m.colour().into(),
        matches!(m, Marker::Open | Marker::Closed),
    )
}

/// A link out: the system browser where there is one, the page's own link
/// on the web (`data_layers::open_url`, as egui's card).
fn link(acts: &Rc<RefCell<Vec<Act>>>, label: String, url: String, sub: View) -> View {
    if crate::systems::data_layers::open_url_supported() {
        let click = on(acts, Act::Url(url));
        view! {
            <ListItem icon={icon(icons::OPEN_EXTERNAL)} sub={sub} onclick={Some(click)}>{label}</ListItem>
        }
    } else {
        view! {
            <ListItem icon={icon(icons::OPEN_EXTERNAL)} sub={sub} href={Some(url)}>{label}</ListItem>
        }
    }
}

/// One block of the card.
fn block(b: Block, acts: &Rc<RefCell<Vec<Act>>>) -> View {
    match b {
        Block::Title(text) => view! {
            <p style="margin: 0; font-size: 17px; font-weight: 600; line-height: 1.35; color: var(--m-text-strong)">{text}</p>
        },
        Block::Line {
            text,
            ink: i,
            size: sz,
            gap,
        } => {
            let style = format!(
                "margin: {} 0 0; color: {}; font-size: {}; line-height: 1.45",
                if gap { "4px" } else { "0" },
                ink(i),
                size(sz)
            );
            view! { <p style={style}>{text}</p> }
        }
        Block::Link { label, url, note } => {
            let sub: View = match note {
                Some(n) => n.into_view(),
                None => View::Empty,
            };
            link(acts, label, url, sub)
        }
        Block::Meeting { date, label, url } => link(acts, label, url, date.into_view()),
        Block::Section(text) => view! {
            <p style="margin: 12px 0 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase">{text}</p>
        },
        Block::Row {
            marker: m,
            text,
            go,
        } => {
            let click = on(acts, Act::Card(CardAct::Go(go)));
            view! {
                <ListItem icon={marker(m)} onclick={Some(click)}>{text}</ListItem>
            }
        }
        Block::Follow { at, followed } => {
            let (variant, label) = if followed {
                (Variant::Primary, "Following · stop")
            } else {
                (Variant::Secondary, "Follow this place")
            };
            let click = on(acts, Act::Card(CardAct::Follow(at)));
            let explained: View = if followed {
                view! {
                    <p style="margin: 0; color: var(--m-text-dim); font-size: 12px; line-height: 1.45">{follow_explained()}</p>
                }
            } else {
                View::Empty
            };
            view! {
                <div style="display: flex; flex-direction: column; gap: 8px; margin-top: 12px">
                    <Button variant={variant} icon={icon(icons::FLAG)} onclick={Some(click)} title={follow_tip()}>{label}</Button>
                    {explained}
                </div>
            }
        }
    }
}

/// The card: its kicker and ✕, then the blocks.
fn card_view(s: Signal<CardState>, acts: &Rc<RefCell<Vec<Act>>>) -> View {
    let frame = {
        let s = s.clone();
        move || s.get().place.style()
    };
    let kicker = {
        let s = s.clone();
        move || -> View {
            match s.get().model.kicker {
                Some((text, i)) => {
                    let style = format!(
                        "flex: 1; min-width: 0; margin: 0; color: {}; font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase",
                        ink(i)
                    );
                    view! { <p style={style}>{text}</p> }
                }
                None => view! { <span style="flex: 1"></span> },
            }
        }
    };
    let body = {
        let s = s.clone();
        let acts = acts.clone();
        move || -> View {
            match s.get().model.blocks {
                Some(blocks) => {
                    let views: Vec<View> = blocks.into_iter().map(|b| block(b, &acts)).collect();
                    views.into_view()
                }
                None => view! {
                    <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                        <Spinner size=Size::Sm />"Reading what is published…"
                    </div>
                },
            }
        }
    };
    let close = on(acts, Act::Close);
    // The card's own node listens, so the pointer over its padding is the
    // library's too (`UiPointer` counts only nodes that listen).
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <div style="flex: none; display: flex; align-items: center; gap: 12px; min-height: 52px; padding: 4px 12px 0 16px">
                <span style="color: var(--m-accent)">{icon(icons::FLAG)}</span>
                {kicker}
                <CloseButton onclick={Some(close)} />
            </div>
            <div style="min-height: 0; overflow-y: auto; padding: 4px 16px 20px; display: flex; flex-direction: column; gap: 8px">
                {body}
            </div>
        </div>
    }
}
