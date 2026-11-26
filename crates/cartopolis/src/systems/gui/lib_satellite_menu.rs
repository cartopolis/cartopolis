//! What a tap on a satellite opens, drawn by the component library
//! (`lib_host`), beside egui's (`satellites::render_satellite_menu`): the
//! ring of actions round it — Follow, Path, More — through the map ring's
//! [`RingMount`](crate::systems::lib_action_ring), and, from More, the card
//! with everything known about it.
//!
//! On with `CARTO_UI_LIB=satellite_menu` (or `1`), on the web `?ui=lib`.
//! egui's system steps aside (`SatelliteLayer::ring_lib.on`,
//! `SatelliteLayer::card_lib`) but keeps every decision — which satellite,
//! where its ring stands, what the card says — and publishes it in
//! `ring_lib.view` and `card_view`. A tap here goes back (`ring_lib.acts`,
//! `card_acts`) and is done by the code egui's buttons run, next frame.
//!
//! **Placement.** As egui's card: centred over the bottom edge,
//! `dock::reserved_bottom` + 8 up, `card_view.width` wide inside egui's
//! popup margin of 10.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{Res, ResMut};
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::lib_action_ring::{sync_ring, RingMount};
use crate::systems::lib_pins::px_scale;
use crate::systems::lib_place_card::icon;
use crate::systems::lib_settings::LibScreens;
use crate::systems::satellites::{SatCardAct, SatCardView, SatelliteLayer};
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The satellite's ring's name in the library's host.
const RING_KEY: &str = "satellite_ring";

/// The card's name in the library's host.
const CARD_KEY: &str = "satellite_card";

/// egui's popup margin round the card's contents (`theme::style`'s
/// `menu_margin`), points.
const MARGIN: f32 = 10.0;

/// What the card shows that does not change as the satellite moves.
#[derive(Clone, PartialEq)]
struct Head {
    name: String,
    kind: &'static str,
    swatch: [u8; 3],
    riding: bool,
}

/// Where the card stands: its bottom above the window's and its width,
/// points.
#[derive(Clone, Copy, PartialEq)]
struct Place {
    bottom: f32,
    width: f32,
}

struct CardMount {
    dark: bool,
    k: f32,
    head: Signal<Head>,
    rows: Signal<Vec<(&'static str, String)>>,
    place: Signal<Place>,
    acts: Rc<RefCell<Vec<SatCardAct>>>,
}

thread_local! {
    static RING: RefCell<Option<RingMount>> = const { RefCell::new(None) };
    static CARD: RefCell<Option<CardMount>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the satellite's ring and card. `PostUpdate`,
/// before `DriveUi`, as every library screen.
pub fn drive_lib_satellite_menu(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    layer: Option<ResMut<SatelliteLayer>>,
    metrics: bevy::prelude::Res<UiMetrics>,
) {
    if !lib.satellite_menu {
        return;
    }
    let Some(mut layer) = layer else {
        return;
    };
    if !layer.ring_lib.on {
        layer.ring_lib.on = true;
    }
    if !layer.card_lib {
        layer.card_lib = true;
    }
    let k = px_scale(metrics.ui_scale());
    RING.with(|m| sync_ring(m, RING_KEY, &mut layer.ring_lib, k));

    let dark = theme::p().ink == theme::DARK.ink;
    CARD.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, for egui's system to do next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<SatCardAct> = std::mem::take(&mut *live.acts.borrow_mut());
            if !acts.is_empty() {
                layer.card_acts.extend(acts);
            }
        }
        let stale = m
            .as_ref()
            .is_some_and(|l| layer.card_view.is_none() || l.dark != dark || l.k != k);
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(CARD_KEY);
        }
        let Some(view) = layer.card_view.as_ref() else {
            return;
        };
        let (head, rows, place) = split(view);
        match m.as_ref() {
            None => {
                let live = CardMount {
                    dark,
                    k,
                    head: signal(head),
                    rows: signal(rows),
                    place: signal(place),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                crate::systems::lib_host::mount_keyed(
                    CARD_KEY,
                    card_view(&live),
                    dark,
                    theme::touch_layout(),
                );
                *m = Some(live);
            }
            Some(live) => {
                if live.head.peek(|now| *now != head) {
                    live.head.set(head);
                }
                if live.rows.peek(|now| *now != rows) {
                    live.rows.set(rows);
                }
                if live.place.get_untracked() != place {
                    live.place.set(place);
                }
            }
        }
    });
}

/// The card's view in its three parts, each its own signal.
fn split(v: &SatCardView) -> (Head, Vec<(&'static str, String)>, Place) {
    (
        Head {
            name: v.name.clone(),
            kind: v.kind,
            swatch: v.swatch,
            riding: v.riding,
        },
        v.rows.clone(),
        Place {
            bottom: v.bottom,
            width: v.width,
        },
    )
}

/// The card: the kind's dot, the name and ✕; the kind; the rows; Follow or
/// Let go. egui's popup frame, centred over the bottom edge.
fn card_view(l: &CardMount) -> View {
    let k = l.k;
    let band = {
        let place = l.place.clone();
        move || {
            format!(
                "position: absolute; left: 0; right: 0; bottom: {:.1}px; \
                 display: flex; justify-content: center; pointer-events: none",
                place.get().bottom * k
            )
        }
    };
    // egui's `set_width` is the contents': the margin and the rim go round it.
    let frame = {
        let place = l.place.clone();
        move || {
            format!(
                "box-sizing: border-box; width: {:.1}px; padding: {:.1}px; \
                 display: flex; flex-direction: column; gap: {:.1}px; \
                 background: var(--m-surface); border: 1px solid var(--m-border); \
                 border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg); \
                 pointer-events: auto",
                (place.get().width + 2.0 * MARGIN) * k + 2.0,
                MARGIN * k,
                8.0 * k
            )
        }
    };
    let head = {
        let (head, acts) = (l.head.clone(), l.acts.clone());
        move || head_view(&head.get(), k, &acts)
    };
    let rows = {
        let rows = l.rows.clone();
        move || rows_view(&rows.get(), k)
    };
    let button = {
        let (head, acts) = (l.head.clone(), l.acts.clone());
        move || {
            let text = if head.get().riding {
                "Let go"
            } else {
                "Follow"
            };
            let ride = act(&acts, SatCardAct::Ride);
            let wrap = format!("display: flex; margin-top: {:.1}px", 6.0 * k);
            view! {
                <div style={wrap}>
                    <Button size=Size::Sm variant=Variant::Ghost icon={icon(icons::FOLLOW)} onclick={ride}>{text}</Button>
                </div>
            }
        }
    };
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too.
    view! {
        <div style={band}>
            <div style={frame} on:click={|_: &Event| {}}>
                {head}
                {rows}
                {button}
            </div>
        </div>
    }
}

/// A tap that hands `a` back.
fn act(acts: &Rc<RefCell<Vec<SatCardAct>>>, a: SatCardAct) -> Option<Handler> {
    let acts = acts.clone();
    Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
}

/// The kind's dot and the name, ✕ at the end; the kind's name under them.
fn head_view(h: &Head, k: f32, acts: &Rc<RefCell<Vec<SatCardAct>>>) -> View {
    let [r, g, b] = h.swatch;
    // The kind's colour on the map: content, not chrome.
    let dot = format!(
        "flex: none; width: {:.1}px; height: {:.1}px; border-radius: 50%; \
         background: #{r:02x}{g:02x}{b:02x}",
        9.0 * k,
        9.0 * k,
    );
    let dot_box = format!(
        "flex: none; width: {s:.1}px; height: {s:.1}px; display: flex; \
         align-items: center; justify-content: center",
        s = 10.0 * k
    );
    let name = format!(
        "flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; \
         color: var(--m-text-strong); font-size: {:.1}px; font-weight: 600",
        theme::SIZE_TITLE * k
    );
    let kind = format!(
        "color: var(--m-text-dim); font-size: {:.1}px",
        theme::SIZE_SUB * k
    );
    let row = format!("display: flex; align-items: center; gap: {:.1}px", 8.0 * k);
    let (title, label) = (h.name.clone(), h.kind);
    let block = format!(
        "display: flex; flex-direction: column; gap: {:.1}px",
        8.0 * k
    );
    view! {
        <div style={block}>
            <div style={row}>
                <div style={dot_box}><div style={dot}></div></div>
                <span style={name}>{title}</span>
                <CloseButton onclick={act(acts, SatCardAct::Close)} />
            </div>
            <span style={kind}>{label}</span>
        </div>
    }
}

/// egui's two-column grid: the labels dim, 12 apart from their values, the
/// rows 2 apart; 4 more above it than the gap.
fn rows_view(rows: &[(&'static str, String)], k: f32) -> View {
    let col = format!(
        "display: flex; flex-direction: column; gap: {:.1}px",
        2.0 * k
    );
    let keys: Vec<View> = rows
        .iter()
        .map(|(key, _)| {
            let key: &'static str = *key;
            view! { <span style="color: var(--m-text-dim); white-space: nowrap">{key}</span> }
        })
        .collect();
    let values: Vec<View> = rows
        .iter()
        .map(|(_, value)| {
            let value = value.clone();
            view! { <span style="color: var(--m-text); white-space: nowrap">{value}</span> }
        })
        .collect();
    let grid = format!(
        "display: flex; gap: {:.1}px; margin-top: {:.1}px",
        12.0 * k,
        4.0 * k
    );
    let (keys_col, values_col) = (col.clone(), col);
    view! {
        <div style={grid}>
            <div style={keys_col}>{keys}</div>
            <div style={values_col}>{values}</div>
        </div>
    }
}
