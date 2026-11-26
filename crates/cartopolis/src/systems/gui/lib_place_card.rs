//! The place card drawn by the component library (`lib_host`): what a
//! picked search result opens, a card over the map rather than a panel.
//!
//! `place_card::drive_place_card` does the work the card cannot — spending
//! the pick, the suppression (the keyboard, the driver bar, the menu, an
//! open panel on a phone, the route planner, the Share sheet), the camera's
//! distance and the session — and hands it over in `PlaceCard::lib_view`. A
//! verb tapped here goes back the same way and is done by `PlaceCard::act`.
//!
//! **Placement.** Centred over the bottom edge, above the dock
//! (`dock::reserved_bottom`), as wide as the card or the screen less its
//! margins. The library's root is the whole window, so the card is one
//! absolutely placed node whose left, bottom and width follow the viewport
//! each frame ([`Placement`]). A click on it is held off the world by
//! `lib_host::bridge_input` (`UiPointer`), and the card's own node listens,
//! so its padding counts as well as its buttons.
//!
//! The grabber is a tap, not a drag (the library has no sheet gesture): it
//! and More both raise the card to its full rest, where More's two entries
//! are rows.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::lib_settings::LibScreens;
use crate::systems::motion::Rest;
use crate::systems::place_card::{CardPlace, PlaceAction, PlaceCard};
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The card's name in the library's host.
const KEY: &str = "place_card";

/// Width of the card, px, before the screen narrows it: the owner asked for
/// calm, roomy cards, and 20 px edges inside.
pub(crate) const CARD_WIDTH: f32 = 360.0;

/// Where a card over the map stands, px in the library's root: centred over
/// the bottom edge, above the dock.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub(crate) struct Placement {
    pub left: f32,
    pub bottom: f32,
    pub width: f32,
    pub max_height: f32,
}

impl Placement {
    /// For a viewport of `w` × `h` UI points (`UiMetrics`). The library's px
    /// are UI points on the Bevy backend (`lib_host::prepare_bevy` sets
    /// `UiScale` to the UI scale); as DOM they are the page's, `ui_scale`
    /// times larger.
    pub(crate) fn over_the_dock(w: f32, h: f32, ui_scale: f32) -> Self {
        let width = CARD_WIDTH.min(w - 2.0 * theme::OSD_MARGIN).max(0.0);
        let bottom = crate::systems::dock::reserved_bottom() + 8.0;
        let top = crate::systems::dock::reserved_top();
        let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            ui_scale
        } else {
            1.0
        };
        Placement {
            left: ((w - width) / 2.0).max(0.0) * k,
            bottom: bottom * k,
            width: width * k,
            max_height: (h - bottom - top).max(160.0) * k,
        }
    }

    /// The card's frame: the library's card surface, at this place.
    pub(crate) fn style(&self) -> String {
        format!(
            "position: absolute; left: {:.0}px; bottom: {:.0}px; width: {:.0}px; max-height: {:.0}px; \
             display: flex; flex-direction: column; background: var(--m-surface); \
             border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); \
             box-shadow: var(--m-shadow-lg); overflow: hidden",
            self.left, self.bottom, self.width, self.max_height
        )
    }
}

/// An icon glyph in the icon face, as every library screen sets one.
pub(crate) fn icon(glyph: &'static str) -> View {
    view! {
        <span style="font-family: 'Material Symbols Rounded'; font-size: 20px; line-height: 1">{glyph}</span>
    }
}

/// The grabber: a tap raises or lowers the card.
pub(crate) fn grabber(on: Handler) -> View {
    view! {
        <div title="Expand or collapse" on:click={move |e: &Event| on(e)} style="display: flex; justify-content: center; padding: 10px 0 4px">
            <div style="width: 36px; height: 4px; border-radius: var(--m-radius-full); background: var(--m-border-strong)"></div>
        </div>
    }
}

/// The card's header, as lib_search's: the accent icon, the title, ✕.
pub(crate) fn header(glyph: &'static str, title: String, close: Handler) -> View {
    view! {
        <div style="display: flex; align-items: center; gap: 12px; min-height: 56px; padding: 0 12px 0 16px">
            <span style="color: var(--m-accent)">{icon(glyph)}</span>
            <h1 style="flex: 1; min-width: 0; margin: 0; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{title}</h1>
            <CloseButton onclick={Some(close)} />
        </div>
    }
}

/// A cell of an equal-width row of buttons: the button stretched across it.
pub(crate) fn cell(button: View) -> View {
    view! {
        <div style="flex: 1; min-width: 0; display: flex; flex-direction: column">{button}</div>
    }
}

/// What a tap asked for; done by the system.
#[derive(Clone, Copy)]
enum Act {
    /// A verb, handed to `drive_place_card` (`PlaceView::action`).
    Do(PlaceAction),
    /// The grabber or More: the full rest, or back to the peek.
    Toggle,
    Close,
}

/// What changes while the card is up.
#[derive(Clone, PartialEq, Default)]
struct State {
    distance: Option<String>,
    online: bool,
    saved: bool,
    full: bool,
}

#[derive(Clone)]
struct Live {
    /// The place the card was mounted for: another pick mounts it again.
    place: CardPlace,
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

fn state_of(card: &PlaceCard) -> State {
    State {
        distance: card.lib_view.distance.clone(),
        online: card.lib_view.online,
        saved: card.lib_view.saved,
        full: card.rest == Rest::Full,
    }
}

/// Mounts, bridges and unmounts the place card. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_place_card(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut card: ResMut<PlaceCard>,
    metrics: Res<UiMetrics>,
) {
    if !lib.place_card {
        return;
    }
    let size = metrics.viewport_pts();
    let placement = Placement::over_the_dock(size.x, size.y, metrics.ui_scale());
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, before anything else moves.
        if let Some(live) = m.as_ref() {
            let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
            for a in acts {
                match a {
                    Act::Do(action) => card.lib_view.action = Some(action),
                    Act::Toggle => {
                        card.rest = if card.rest == Rest::Full {
                            Rest::Peek
                        } else {
                            Rest::Full
                        };
                    }
                    Act::Close => card.close(),
                }
            }
        }
        let open = card.place.is_some() && !card.lib_view.hidden;
        // Gone, hidden, or another place picked while one was up.
        let stale = m
            .as_ref()
            .is_some_and(|live| !open || card.place.as_ref() != Some(&live.place));
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        if !open {
            return;
        }
        match m.as_mut() {
            None => {
                let Some(place) = card.place.clone() else {
                    return;
                };
                let live = Live {
                    place,
                    state: signal(state_of(&card)),
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
                        warn!(?path, error = %e, "lib place card: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                let state = state_of(&card);
                if live.state.get_untracked() != state {
                    live.state.set(state);
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

/// The card: where a marker's place comes from, the name, the line under
/// it, the verbs, and pulled up what More holds and where the place is.
fn view_of(l: &Live) -> View {
    let place = l.place.clone();
    let frame = {
        let placement = l.placement.clone();
        move || placement.get().style()
    };
    // Where a Community marker's place comes from, on top: the card says
    // which space it belongs to before what it is (N3).
    let origin: View = match &place.from {
        Some(from) => {
            let line = from.line.clone();
            view! {
                <p style="margin: 0; padding: 4px 20px 0; color: var(--m-accent-text); font-size: var(--m-font-size-sm); font-weight: 600">{line}</p>
            }
        }
        None => View::Empty,
    };
    // Kind, distance and context share the dim line under the name, the
    // same shape the suggestion row had, so the card reads as the row it
    // came from.
    let sub = {
        let state = l.state.clone();
        let kind = place.kind.clone();
        let context = place.context.clone();
        move || -> View {
            let distance = state.get().distance;
            if kind.is_none() && distance.is_none() && context.is_empty() {
                return View::Empty;
            }
            let kind: View = match &kind {
                Some(k) => {
                    let k = k.clone();
                    view! { <span style="white-space: nowrap">{k}</span> }
                }
                None => View::Empty,
            };
            let distance: View = match distance {
                Some(d) => view! {
                    <span style="white-space: nowrap; color: var(--m-accent-text)">{d}</span>
                },
                None => View::Empty,
            };
            let context: View = if context.is_empty() {
                View::Empty
            } else {
                let c = context.clone();
                view! { <span>{c}</span> }
            };
            view! {
                <div style="display: flex; flex-wrap: wrap; column-gap: 12px; row-gap: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                    {kind}{distance}{context}
                </div>
            }
        }
    };
    let verbs = {
        let l = l.clone();
        let place = place.clone();
        move || -> View {
            let s = l.state.get();
            // A Community marker: the way into Community is the main verb,
            // and the way there the other (N3). No Save, no Share: it is
            // somebody else's thing, not a place of the map's.
            if let Some(from) = &place.from {
                let verb = from.open.verb();
                return view! {
                    <div style="display: flex; flex-direction: column; gap: 8px">
                        <Button variant=Variant::Primary icon={icon(from.open.icon())} onclick={l.act(Act::Do(PlaceAction::OpenOrigin))}>{verb}</Button>
                        <Button icon={icon(icons::ROUTE)} onclick={l.act(Act::Do(PlaceAction::RouteHere))}>"Route here"</Button>
                    </div>
                };
            }
            let (save_glyph, save_label, save_click) = if s.saved {
                (icons::CHECK, "Saved", None)
            } else {
                (
                    icons::BOOKMARK_ADD,
                    "Save",
                    l.act(Act::Do(PlaceAction::Save)),
                )
            };
            let save = cell(view! {
                <Button size=Size::Sm icon={icon(save_glyph)} onclick={save_click} title="Save this place">{save_label}</Button>
            });
            // Share (bridge ③), the one way a place goes to people; the web
            // build has none, its chats being the page's (`comms`).
            let share = if cfg!(feature = "comms") {
                cell(view! {
                    <Button size=Size::Sm icon={icon(icons::SHARE)} onclick={l.act(Act::Do(PlaceAction::Share))} title="Send this place to a chat or community">"Share"</Button>
                })
            } else {
                View::Empty
            };
            let more_label = if s.full { "Less" } else { "More" };
            let more = cell(view! {
                <Button size=Size::Sm icon={icon(icons::MORE)} onclick={l.act(Act::Toggle)} title="Start a route here, jump there, where it is">{more_label}</Button>
            });
            view! {
                <div style="display: flex; flex-direction: column; gap: 8px">
                    <Button variant=Variant::Primary icon={icon(icons::FINISH)} onclick={l.act(Act::Do(PlaceAction::RouteHere))}>"Route here"</Button>
                    <div style="display: flex; gap: 8px">
                        {save}{share}{more}
                    </div>
                </div>
            }
        }
    };
    // Pulled up: what More holds, as rows, and where the place is.
    let rest = {
        let l = l.clone();
        let place = place.clone();
        move || -> View {
            let s = l.state.get();
            if !s.full || place.from.is_some() {
                return View::Empty;
            }
            // Signed in only: a body in a shared world is what makes "go
            // there" distinct from looking.
            let jump = if s.online {
                view! {
                    <ListItem icon={icon(icons::CAM_FOLLOW)} onclick={l.act(Act::Do(PlaceAction::Jump))}>"Jump there"</ListItem>
                }
            } else {
                View::Empty
            };
            let at = format!("{:.5}, {:.5}", place.lat, place.lng);
            view! {
                <div style="display: flex; flex-direction: column; gap: 12px; padding-top: 16px; border-top: 1px solid var(--m-border)">
                    <List>
                        <ListItem icon={icon(icons::START)} onclick={l.act(Act::Do(PlaceAction::RouteFrom))}>"Start a route here"</ListItem>
                        {jump}
                    </List>
                    <p style="margin: 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{at}</p>
                </div>
            }
        }
    };
    let title = place.name.clone();
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too: `UiPointer` counts
    // only nodes that listen.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            {grabber(l.on(Act::Toggle))}
            {origin}
            {header(icons::PIN, title, l.on(Act::Close))}
            <div style="min-height: 0; overflow-y: auto; padding: 0 16px 20px; display: flex; flex-direction: column; gap: 16px">
                {sub}
                {verbs}
                {rest}
            </div>
        </div>
    }
}
