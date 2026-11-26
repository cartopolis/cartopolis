//! The building card drawn by the component library (`lib_host`), beside
//! egui's (`building_card::render_building_card`): what the registers say
//! about the building you clicked, a card over the map rather than a panel.
//!
//! On with `CARTO_UI_LIB=building_card` (or `1`), on the web `?ui=lib`. egui's
//! system steps aside (`BuildingSelection::card_lib`) but keeps working out
//! what the card shows — `building_card::CardContent`, the same struct its
//! own card is drawn from — and whether something else owns the bottom of
//! the screen, and hands both over in `BuildingSelection::card_bridge`. A
//! verb tapped here goes back the same way and is done by
//! `building_card::run`, the code egui's buttons call.
//!
//! Placed as the place card is (`lib_place_card::Placement`): centred over
//! the bottom edge, above the dock. The grabber is a tap that steps through
//! the three rests (the name and the ways in; the facts; all of them), as
//! the library has no sheet gesture; the card appears and goes without
//! egui's rise.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::building_card::{CardAction, CardContent};
use crate::systems::building_select::BuildingSelection;
use crate::systems::lib_place_card::{grabber, header, icon, Placement};
use crate::systems::lib_settings::LibScreens;
use crate::systems::motion::Rest;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The card's name in the library's host.
const KEY: &str = "building_card";

/// How tall the facts may grow at the middle rest before they scroll, px:
/// egui's figure.
const HALF_ROWS: f32 = 260.0;

/// What a tap asked for; done by the system.
#[derive(Clone, Copy)]
enum Act {
    /// A verb, handed to egui's system (`CardBridge::action`).
    Do(CardAction),
    /// The grabber: the next rest up, and from the top back to the peek.
    Toggle,
    Close,
}

/// What the card draws, as it stands this frame.
#[derive(Clone, PartialEq)]
struct State {
    content: CardContent,
    rest: Rest,
    /// How tall the facts may grow before they scroll, px.
    rows_max: f32,
}

#[derive(Clone)]
struct Live {
    state: Signal<State>,
    placement: Signal<Placement>,
    acts: Rc<RefCell<Vec<Act>>>,
}

impl Live {
    fn on(&self, a: Act) -> Handler {
        let acts = self.acts.clone();
        Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler
    }

    fn act(&self, a: Act) -> Option<Handler> {
        Some(self.on(a))
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the building card. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_building_card(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut selection: ResMut<BuildingSelection>,
    metrics: Res<UiMetrics>,
) {
    if !lib.building_card {
        return;
    }
    if !selection.card_lib {
        selection.card_lib = true;
    }
    let size = metrics.viewport_pts();
    let placement = Placement::over_the_dock(size.x, size.y, metrics.ui_scale());
    let full_rows = size.y
        * 0.5
        * if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            metrics.ui_scale()
        } else {
            1.0
        };
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, before anything else moves.
        if let Some(live) = m.as_ref() {
            let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
            for a in acts {
                match a {
                    Act::Do(action) => selection.card_bridge.action = Some(action),
                    Act::Toggle => {
                        selection.card_rest = match selection.card_rest {
                            Rest::Peek => Rest::Half,
                            Rest::Half => Rest::Full,
                            Rest::Full => Rest::Peek,
                        };
                    }
                    Act::Close => selection.clear(),
                }
            }
        }
        // What egui's system published for this frame; nothing once the
        // selection is gone (the ✕ above takes effect next frame there).
        let content = selection
            .card_bridge
            .content
            .clone()
            .filter(|_| selection.selected.is_some());
        let Some(content) = content else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        };
        let rest = selection.card_rest;
        let state = State {
            content,
            rest,
            rows_max: if rest == Rest::Full {
                full_rows
            } else {
                HALF_ROWS
            },
        };
        match m.as_mut() {
            None => {
                let live = Live {
                    state: signal(state),
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
                        warn!(?path, error = %e, "lib building card: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                if live.state.get_untracked() != state {
                    live.state.set(state);
                    // The facts land over several frames: the HTML beside a
                    // capture follows them.
                    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                        let _ = std::fs::write(&path, html(live));
                    }
                }
                if live.placement.get_untracked() != placement {
                    live.placement.set(placement);
                }
            }
        }
    });
}

/// The card's HTML from the library's string renderer, for the browser
/// beside a capture.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view_of(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

/// One verb's button, as egui's card names and explains it.
fn verb(l: &Live, c: &CardContent, action: CardAction) -> View {
    let click = l.act(Act::Do(action));
    match action {
        CardAction::Share => view! {
            <Button size=Size::Sm icon={icon(icons::SHARE)} onclick={click} title="Send this building to a chat or community">"Share"</Button>
        },
        CardAction::Inside => {
            let (variant, tip) = if c.looking {
                (Variant::Primary, "Stop looking inside")
            } else {
                (Variant::Secondary, "See the floors inside")
            };
            view! {
                <Button size=Size::Sm variant={variant} icon={icon(icons::BUILDINGS)} onclick={click} title={tip}>"Look inside"</Button>
            }
        }
        CardAction::Plan => view! {
            <Button size=Size::Sm icon={icon(icons::OPEN_EXTERNAL)} disabled={!c.can_open} onclick={click} title="Open this building in the floor-plan editor">"Floor plan"</Button>
        },
        CardAction::Replace => view! {
            <Button size=Size::Sm icon={icon(icons::OPEN_EXTERNAL)} disabled={!c.can_open} onclick={click} title="Open this building in the Overrides tab, to put a model in its place">"Replace"</Button>
        },
        CardAction::Wrong => view! {
            <Button size=Size::Sm icon={icon(icons::FLAG)} disabled={c.busy} onclick={click} title="The walls' colour or material is not right: measure them again from street photos">"Facade wrong"</Button>
        },
        CardAction::Photo => view! {
            <Button size=Size::Sm icon={icon(icons::CAMERA)} disabled={c.busy} onclick={click} title="Take a photo of the front: its colour and material are measured from it, and the photo is not kept">"Facade photo"</Button>
        },
        // Not among the paired buttons.
        CardAction::CancelPhoto | CardAction::Record => View::Empty,
    }
}

/// The facts, one name/value row each; a value of several lines (the
/// addresses) a line each.
fn facts(rows: &[(&'static str, String)], max: f32) -> View {
    let rows: Vec<View> = rows
        .iter()
        .map(|(name, value)| {
            let name: &'static str = *name;
            let lines: Vec<View> = value
                .lines()
                .map(|line| {
                    let line = line.to_owned();
                    view! { <span>{line}</span> }
                })
                .collect();
            view! {
                <div style="display: flex; gap: 16px">
                    <span style="flex: none; width: 96px; color: var(--m-text-dim)">{name}</span>
                    <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; color: var(--m-text)">{lines}</div>
                </div>
            }
        })
        .collect();
    let style = format!(
        "max-height: {max:.0}px; overflow-y: auto; display: flex; flex-direction: column; gap: 8px; font-size: var(--m-font-size-sm)"
    );
    view! { <div style={style}>{rows}</div> }
}

/// The card: the street and ✕, the uses, the facts once pulled up, the ways
/// into the building, the facade's answer, and the register's own record.
fn view_of(l: &Live) -> View {
    let frame = {
        let placement = l.placement.clone();
        move || placement.get().style()
    };
    let head = {
        let l = l.clone();
        move || -> View {
            let title = l.state.get().content.title;
            header(icons::BUILDINGS, title, l.on(Act::Close))
        }
    };
    let body = {
        let l = l.clone();
        move || -> View {
            let State {
                content: c,
                rest,
                rows_max,
            } = l.state.get();
            let subtitle: View = if c.subtitle.is_empty() {
                View::Empty
            } else {
                let s = c.subtitle.clone();
                view! {
                    <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{s}</p>
                }
            };
            let rows: View = if rest >= Rest::Half && !c.rows.is_empty() {
                facts(&c.rows, rows_max)
            } else {
                View::Empty
            };
            // Each at its own width, wrapping onto the next line: two equal
            // columns of five verbs did not fit a phone, and the right one
            // stuck out of the card.
            let verbs: Vec<View> = c.actions.iter().map(|a| verb(&l, &c, *a)).collect();
            let pairs: View = view! {
                <div style="display: flex; flex-wrap: wrap; gap: 8px">{verbs}</div>
            };
            let note: View = match &c.note {
                Some(note) => {
                    let note = note.clone();
                    let busy: View = if c.busy {
                        view! { <Spinner size=Size::Sm /> }
                    } else {
                        View::Empty
                    };
                    let cancel: View = if c.capturing {
                        view! {
                            <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(Act::Do(CardAction::CancelPhoto))}>"Cancel"</Button>
                        }
                    } else {
                        View::Empty
                    };
                    view! {
                        <div style="display: flex; align-items: center; gap: 8px">
                            {busy}
                            <span style="flex: 1; min-width: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{note}</span>
                            {cancel}
                        </div>
                    }
                }
                None => View::Empty,
            };
            // Pulled up: the register's own record, and the id itself — the
            // join to every other Dutch register, which somebody will want
            // to copy.
            let record: View = if rest >= Rest::Half {
                let tip = if c.can_open {
                    "The Kadaster's own record of this building"
                } else {
                    "Opening links isn't available on this platform"
                };
                let id = c.id.to_string();
                view! {
                    <div style="display: flex; align-items: center; gap: 12px; padding-top: 16px; border-top: 1px solid var(--m-border)">
                        <Button icon={icon(icons::OPEN_EXTERNAL)} disabled={!c.can_open} onclick={l.act(Act::Do(CardAction::Record))} title={tip}>"Official record"</Button>
                        <span style="flex: 1; min-width: 0; text-align: end; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{id}</span>
                    </div>
                }
            } else {
                View::Empty
            };
            view! {
                {subtitle}
                {rows}
                <div style="display: flex; flex-direction: column; gap: 8px">{pairs}</div>
                {note}
                {record}
            }
        }
    };
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too: `UiPointer` counts
    // only nodes that listen.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            {grabber(l.on(Act::Toggle))}
            {head}
            <div style="min-height: 0; overflow-y: auto; padding: 0 16px 20px; display: flex; flex-direction: column; gap: 16px">
                {body}
            </div>
        </div>
    }
}
