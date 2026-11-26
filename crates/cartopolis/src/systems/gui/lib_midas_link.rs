//! The Midas corner drawn by the component library (`lib_host`), beside
//! egui's (`midas_link::draw_link_button`): the Midas disc while the link is
//! up, the card a tap on it opens (which session asked for what, and as
//! whom), and the bubble with what a session `tell`s the person.
//!
//! On with `CARTO_UI_LIB=midas_link` (or `1`). egui's system steps aside
//! (`MidasLink::lib`) but still decides everything — up or not, under a
//! panel or in driver mode, the glow, the card's words, the bubble's timeout
//! — and publishes it in `MidasLink::lib_view`. A tap here goes back in
//! `MidasLink::lib_acts` and is done next frame by `MidasLink::apply`, the
//! code egui's taps call.
//!
//! **Drawn from boxes.** The disc is `theme::map_disc`'s plate; the Midas
//! mark (`theme::paint_midas_mark`, an M of five joined points) is four
//! thick strokes as boxes turned by `transform: rotate()` about their
//! middles, and a round box on each point; the glow a bordered circle; the
//! linked dot a green circle with an ink rim. The bubble's tail is a box of
//! borders, three clear and the left one inked, as `lib_chrome`'s needle.
//!
//! **Placement.** As egui's: the disc `MidasLink::lib_view.right` in from
//! the right edge (left of the map's round buttons, `dock::right_column`)
//! at `dock::reserved_top`; the card and the bubble in one column left of it,
//! `OSD_GAP` away, the bubble under the card when both are up, its tail at
//! the disc only when nothing stands between.
//!
//! Native only, as the link (`not(wasm32)`): one px per egui point.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{Res, ResMut};
// The library's, not Bevy's.
use mithril::prelude::Event;
use mithril::prelude::*;

use crate::systems::lib_settings::LibScreens;
use crate::systems::midas_link::{LinkAct, LinkCardView, MidasLink, ToldView};
use crate::systems::theme;
use crate::systems::theme::tokens::{alpha, hex};

/// The corner's name in the library's host.
const KEY: &str = "midas_link";

/// The disc's diameter: the one disc the map wears.
const DISC: f32 = theme::ROUND_BUTTON_SIZE;

/// The card's and the bubble's content width, egui's figure.
const CARD_WIDTH: f32 = 230.0;

/// egui's `overlay_frame` across: both paddings (14) and rims (1).
const FRAME_X: f32 = 2.0 * 14.0 + 2.0;

type Acts = Rc<RefCell<Vec<LinkAct>>>;

struct Live {
    /// The theme it was mounted in: the gold, the ink and the green are
    /// baked into the styles, so another theme mounts it again.
    dark: bool,
    acts: Acts,
    /// The disc's (right, top), egui points.
    place: Signal<(f32, f32)>,
    glow: Signal<Option<f32>>,
    card: Signal<Option<LinkCardView>>,
    told: Signal<Option<ToldView>>,
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

/// A tap that hands `a` back.
fn tap(acts: &Acts, a: LinkAct) -> impl Fn(&Event) + 'static {
    let acts = acts.clone();
    move |_: &Event| acts.borrow_mut().push(a)
}

/// Mounts, bridges and unmounts the corner. `PostUpdate`, before `DriveUi`,
/// as every library screen.
pub fn drive_lib_midas_link(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut link: ResMut<MidasLink>,
) {
    if !lib.midas_link {
        return;
    }
    if !link.lib {
        link.lib = true;
    }
    let dark = theme::p().ink == theme::DARK.ink;
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, for egui's system to do next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<LinkAct> = std::mem::take(&mut *live.acts.borrow_mut());
            if !acts.is_empty() {
                link.lib_acts.extend(acts);
            }
        }
        let stale = m
            .as_ref()
            .is_some_and(|live| link.lib_view.is_none() || live.dark != dark);
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let Some(view) = link.lib_view.clone() else {
            return;
        };
        let live = m.get_or_insert_with(|| {
            let live = Live {
                dark,
                acts: Rc::new(RefCell::new(Vec::new())),
                place: signal((view.right, view.top)),
                glow: signal(view.glow),
                card: signal(view.card.clone()),
                told: signal(view.told.clone()),
            };
            crate::systems::lib_host::mount_keyed(KEY, view_of(&live), dark, theme::touch_layout());
            live
        });
        put(&live.place, (view.right, view.top));
        put(&live.glow, view.glow);
        put(&live.card, view.card);
        put(&live.told, view.told);
    });
}

/// `v` as px.
fn px(v: f32) -> String {
    format!("{v:.1}px")
}

/// The root: the whole window, listening nowhere; the disc, and the column
/// of card and bubble left of it.
fn view_of(l: &Live) -> View {
    let root = format!(
        "position: absolute; left: 0; top: 0; right: 0; bottom: 0; pointer-events: none; {}",
        theme::map_vars()
    );
    let disc = disc_view(l);
    let column_style = {
        let place = l.place.clone();
        move || {
            let (right, top) = place.get();
            format!(
                "position: absolute; right: {}; top: {}; width: {}; display: flex; \
                 flex-direction: column; gap: {}; pointer-events: none",
                px(right + DISC + theme::OSD_GAP),
                px(top),
                px(CARD_WIDTH + FRAME_X),
                px(theme::OSD_GAP),
            )
        }
    };
    let card = {
        let (card, acts) = (l.card.clone(), l.acts.clone());
        move || -> View {
            match card.get() {
                Some(c) => card_view(&c, &acts),
                None => View::Empty,
            }
        }
    };
    let told = {
        let (told, card, acts) = (l.told.clone(), l.card.clone(), l.acts.clone());
        move || -> View {
            match told.get() {
                // The tail only with nothing between it and the disc.
                Some(t) => told_view(&t, card.get().is_none(), &acts),
                None => View::Empty,
            }
        }
    };
    view! {
        <div style={root}>
            {disc}
            <div style={column_style}>
                {card}
                {told}
            </div>
        </div>
    }
}

// ─── The disc ────────────────────────────────────────────────────────────────

/// A round box of `r` centred on (`x`, `y`) in the disc's square.
fn dot(x: f32, y: f32, r: f32, fill: &str, extra: &str) -> View {
    let style = format!(
        "position: absolute; left: {}; top: {}; width: {d}; height: {d}; box-sizing: border-box; \
         border-radius: var(--m-radius-full); background: {fill}{extra}",
        px(x - r),
        px(y - r),
        d = px(2.0 * r),
    );
    view! { <span style={style}></span> }
}

/// A stroke `w` thick from `a` to `b`: a box as long as the stroke, laid on
/// its middle and turned to its angle.
fn stroke(a: (f32, f32), b: (f32, f32), w: f32, ink: &str) -> View {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    let (mx, my) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let style = format!(
        "position: absolute; left: {}; top: {}; width: {}; height: {}; background: {ink}; \
         transform: rotate({:.1}deg)",
        px(mx - len / 2.0),
        px(my - w / 2.0),
        px(len),
        px(w),
        dy.atan2(dx).to_degrees(),
    );
    view! { <span style={style}></span> }
}

/// The Midas mark on a disc of `radius` centred at `c` in the square:
/// `theme::paint_midas_mark`'s geometry, the icon's 64-unit square.
fn midas_mark(c: f32, radius: f32) -> View {
    let gold = hex(theme::midas_gold());
    let s = radius / 26.0;
    let at = |x: f32, y: f32| (c + (x - 32.0) * s, c + (y - 32.0) * s);
    let points = [
        at(20.0, 43.0),
        at(20.0, 21.0),
        at(32.0, 33.0),
        at(44.0, 21.0),
        at(44.0, 43.0),
    ];
    let mut parts: Vec<View> = points
        .windows(2)
        .map(|w| stroke(w[0], w[1], 5.0 * s, &gold))
        .collect();
    parts.extend(points.iter().map(|&(x, y)| dot(x, y, 4.0 * s, &gold, "")));
    View::Fragment(parts)
}

/// The disc: the plate, the glow while a call is fresh, the mark, the green
/// dot that says linked. A tap opens or closes the card.
fn disc_view(l: &Live) -> View {
    let spot = {
        let place = l.place.clone();
        move || {
            let (right, top) = place.get();
            format!(
                "position: absolute; right: {}; top: {}; width: {s}; height: {s}; \
                 pointer-events: auto; cursor: pointer",
                px(right),
                px(top),
                s = px(DISC),
            )
        }
    };
    let plate = format!(
        "position: absolute; left: 0; top: 0; width: {s}; height: {s}; box-sizing: border-box; \
         border-radius: var(--m-radius-full); border: 1.5px solid var(--map-rim); \
         background: var(--map-fill); box-shadow: var(--m-shadow)",
        s = px(DISC),
    );
    let c = DISC / 2.0;
    let radius = DISC / 2.0 - 1.0;
    let glow = {
        let glow = l.glow.clone();
        move || -> View {
            let Some(g) = glow.get() else {
                return View::Empty;
            };
            // egui's ring: radius + 2 + 6(1 − g), 3g wide, the gold at g.
            let ring = radius + 2.0 + 6.0 * (1.0 - g);
            let w = 3.0 * g;
            dot(
                c,
                c,
                ring + w / 2.0,
                "transparent",
                &format!(
                    "; border: {} solid {}",
                    px(w),
                    alpha(theme::midas_gold(), g * 100.0)
                ),
            )
        }
    };
    let mark = midas_mark(c, radius);
    let linked = dot(
        c + radius * 0.74,
        c - radius * 0.74,
        5.75,
        &hex(theme::CALL_ACCEPT_GREEN),
        &format!("; border: 1.5px solid {}", hex(theme::p().ink)),
    );
    el("div")
        .attr("style", spot)
        .attr("title", "Midas")
        .attr("aria-label", "Midas")
        .on("click", tap(&l.acts, LinkAct::Toggle))
        .child(view! { <span style={plate}></span> })
        .child(glow)
        .child(mark)
        .child(linked)
        .into_view()
}

// ─── The card and the bubble ─────────────────────────────────────────────────

/// egui's `overlay_frame` in `rim`, as a style: border-box, so its width is
/// the column's.
fn frame(rim: &str, gap: f32) -> String {
    format!(
        "position: relative; box-sizing: border-box; padding: 12px 14px; display: flex; \
         flex-direction: column; gap: {}; background: var(--m-surface); border: 1px solid {rim}; \
         border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow); pointer-events: auto; \
         cursor: pointer",
        px(gap)
    )
}

/// "Midas linked", the last call in gold, and as whom. A tap closes it.
fn card_view(c: &LinkCardView, acts: &Acts) -> View {
    let title = format!(
        "font-size: {}px; font-weight: 600; color: var(--m-text-strong)",
        theme::SIZE_SUB
    );
    let call_style = format!(
        "font-size: {}px; color: {}",
        theme::SIZE_CHIP,
        hex(theme::midas_gold())
    );
    let acts_style = format!(
        "font-size: {}px; color: var(--m-text-dim)",
        theme::SIZE_TINY
    );
    let call = c.call.clone();
    let who = c.acts.clone();
    el("div")
        .attr("style", frame("var(--m-border)", 3.0))
        .attr("aria-label", "Close")
        .on("click", tap(acts, LinkAct::CloseCard))
        .child(view! { <span style={title}>"Midas linked"</span> })
        .child(view! { <span style={call_style}>{call}</span> })
        .child(view! { <span style={acts_style}>{who}</span> })
        .into_view()
}

/// The session in gold and what it said; the tail at the disc when `tail`.
/// A tap takes it down.
fn told_view(t: &ToldView, tail: bool, acts: &Acts) -> View {
    let gold = theme::midas_gold();
    let session_style = format!(
        "font-size: {}px; font-weight: 600; color: {}",
        theme::SIZE_CHIP,
        hex(gold)
    );
    let text_style = format!(
        "font-size: {}px; color: var(--m-text-strong); overflow-wrap: anywhere",
        theme::SIZE_BODY
    );
    let tail: View = if tail {
        // Level with the disc's centre, across the gap, over the rim as
        // egui's: from the right edge less 1 px, in the frame's padding box.
        let style = format!(
            "position: absolute; left: {}; top: {}; width: {}; height: 14px; box-sizing: border-box; \
             border-style: solid; border-width: 7px 0 7px {}; border-color: transparent; \
             border-left-color: var(--m-surface)",
            px(CARD_WIDTH + 28.0 - 1.0),
            px(DISC / 2.0 - 7.0 - 1.0),
            px(theme::OSD_GAP),
            px(theme::OSD_GAP),
        );
        view! { <span style={style}></span> }
    } else {
        View::Empty
    };
    let session = t.session.clone();
    let text = t.text.clone();
    el("div")
        .attr("style", frame(&alpha(gold, 60.0), 4.0))
        .attr("aria-label", "Dismiss")
        .on("click", tap(acts, LinkAct::DismissTold))
        .child(view! { <span style={session_style}>{session}</span> })
        .child(view! { <span style={text_style}>{text}</span> })
        .child(tail)
        .into_view()
}
