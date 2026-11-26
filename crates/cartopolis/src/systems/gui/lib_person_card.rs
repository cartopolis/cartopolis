//! The person card and the first-message sheet drawn by the component
//! library (`lib_host`), beside egui's (`contacts::render_person_card`): who
//! the avatar you tapped is, and the one line you send them with a request.
//!
//! On with `CARTO_UI_LIB=person_card` (or `1`), on the web `?ui=lib`. egui's
//! system steps aside (`Contacts::lib`) but keeps the work only it can do —
//! whether the session, the driver bar or an open panel hides the card, how
//! far the on-screen keyboard lifts the sheet — and hands it over in
//! `Contacts::lib_view`. A tap here goes back the same way and is done by
//! `contacts::person_act`, the code egui's buttons call. The sheet's text is
//! diffed into `Compose::text` each frame, as Search's field is.
//!
//! **Placement.** As egui's: centred over the bottom edge, above the dock
//! (`lib_place_card::Placement`), the sheet lifted over the keyboard. The
//! avatar is the person's picture (`lib_chats::face_src`), or their initial
//! in a disc while they have none.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
use cartopolis_core::{Greeting, Relation};
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::contacts::{
    hello_fine_print, may_ask, person_facts, relation_word, Contacts, Fact, PersonAction,
    MAX_HELLO_CHARS, QUICK_HELLOS,
};
use crate::systems::lib_place_card::{cell, icon, Placement};
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};
use crate::utils::clock::unix_ms;

/// The card's name in the library's host.
const KEY: &str = "person_card";

/// Which of the two is up: another mounts it again.
#[derive(Clone, PartialEq)]
enum Mode {
    Card,
    /// The first-message sheet, to this person.
    Compose(String),
}

/// The person as the card draws them.
#[derive(Clone, PartialEq)]
struct Shown {
    username: String,
    relation: Relation,
    facts: Vec<Fact>,
    /// Say hi and Add contact are offered.
    may_ask: bool,
    /// The greetings are offered (an avatar tapped, signed in).
    can_greet: bool,
}

/// What the card draws, as it stands this frame.
#[derive(Clone, PartialEq, Default)]
struct CardState {
    looking: bool,
    person: Option<Shown>,
    menu: bool,
    error: Option<String>,
}

/// What a tap asked for; done by the system.
#[derive(Clone, Copy)]
enum Act {
    /// Handed to egui's system (`PersonView::action`).
    Do(PersonAction),
    /// A quick first line, into the sheet's text.
    Quick(usize),
}

#[derive(Clone)]
struct Live {
    mode: Mode,
    card: Signal<CardState>,
    /// The sheet's text.
    text: Signal<String>,
    /// The sheet's error.
    error: Signal<Option<String>>,
    /// Nothing written yet: Send waits.
    empty: Signal<bool>,
    placement: Signal<Placement>,
    acts: Rc<RefCell<Vec<Act>>>,
}

impl Live {
    fn on(&self, a: Act) -> Handler {
        let acts = self.acts.clone();
        Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler
    }

    fn act(&self, a: PersonAction) -> Option<Handler> {
        Some(self.on(Act::Do(a)))
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

fn card_state(contacts: &Contacts, now: u64) -> CardState {
    let Some(card) = contacts.card.as_ref() else {
        return CardState::default();
    };
    CardState {
        looking: card.looking,
        person: card.person.as_ref().map(|p| Shown {
            username: p.username.clone(),
            relation: p.relation,
            facts: person_facts(p, now),
            may_ask: may_ask(p, card),
            can_greet: card.avatar_id.is_some()
                && p.relation != Relation::Me
                && contacts.lib_view.online,
        }),
        menu: card.menu,
        error: contacts.error.clone(),
    }
}

/// Mounts, bridges and unmounts the person card and the sheet. `PostUpdate`,
/// before `DriveUi`, as every library screen.
pub fn drive_lib_person_card(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut contacts: ResMut<Contacts>,
    metrics: Res<UiMetrics>,
) {
    if !lib.person_card {
        return;
    }
    let size = metrics.viewport_pts();
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        metrics.ui_scale()
    } else {
        1.0
    };
    let now = unix_ms();
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(live) = m.as_ref() {
            // The sheet's field → the draft, as egui's `TextEdit` writes it.
            if let Mode::Compose(_) = live.mode {
                let typed = live.text.get_untracked();
                if contacts.compose.as_ref().is_some_and(|c| c.text != typed) {
                    if let Some(c) = contacts.compose.as_mut() {
                        c.text = typed;
                    }
                }
            }
            // The taps of the frame.
            let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
            for a in acts {
                match a {
                    Act::Do(action) => contacts.lib_view.action = Some(action),
                    Act::Quick(i) => {
                        if let (Some(c), Some(line)) =
                            (contacts.compose.as_mut(), QUICK_HELLOS.get(i))
                        {
                            c.text = (*line).to_string();
                        }
                    }
                }
            }
        }
        let mode = if contacts.lib_view.hidden {
            None
        } else if let Some(c) = contacts.compose.as_ref() {
            Some(Mode::Compose(c.username.clone()))
        } else if contacts.card.is_some() && !contacts.lib_view.card_hidden {
            Some(Mode::Card)
        } else {
            None
        };
        let stale = m
            .as_ref()
            .is_some_and(|live| Some(&live.mode) != mode.as_ref());
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let Some(mode) = mode else {
            return;
        };
        let mut placement = Placement::over_the_dock(size.x, size.y, metrics.ui_scale());
        if let Mode::Compose(_) = mode {
            placement.bottom += contacts.lib_view.lift * k;
        }
        let text = contacts
            .compose
            .as_ref()
            .map(|c| c.text.clone())
            .unwrap_or_default();
        let card = card_state(&contacts, now);
        match m.as_mut() {
            None => {
                let live = Live {
                    mode: mode.clone(),
                    card: signal(card),
                    empty: signal(text.trim().is_empty()),
                    text: signal(text),
                    error: signal(contacts.error.clone()),
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
                        warn!(?path, error = %e, "lib person card: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                // The person lands after the mount: the HTML beside a capture
                // follows them.
                if live.card.get_untracked() != card {
                    live.card.set(card);
                    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                        let _ = std::fs::write(&path, html(live));
                    }
                }
                // A quick line or a send moved the draft → the field.
                if live.text.get_untracked() != text {
                    live.text.set(text.clone());
                }
                let empty = text.trim().is_empty();
                if live.empty.get_untracked() != empty {
                    live.empty.set(empty);
                }
                if live.error.get_untracked() != contacts.error {
                    live.error.set(contacts.error.clone());
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

/// The person's picture where egui draws their avatar, round
/// (`lib_chats::face_src`); their initial in an accent disc while they have
/// none, or it is on its way.
fn initial(name: &str, size: f32) -> View {
    let name = name.to_string();
    let px = size.round() as u32;
    (move || -> View {
        match crate::systems::lib_chats::face_src(&name, px) {
            Some(src) => crate::systems::lib_chats::face_img(&src, &name, px),
            None => letter_disc(&name, size),
        }
    })
    .into_view()
}

/// The person's initial in an accent disc.
fn letter_disc(name: &str, size: f32) -> View {
    let letter: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    let style = format!(
        "flex: none; width: {size:.0}px; height: {size:.0}px; border-radius: var(--m-radius-full); \
         background: var(--m-accent-soft); color: var(--m-accent-text); display: flex; \
         align-items: center; justify-content: center; font-size: {:.0}px; font-weight: 600",
        size * 0.42
    );
    view! { <div style={style}>{letter}</div> }
}

/// One small fact, a tag as egui's (`contacts::fact`): teal, or amber for
/// the new account.
fn fact(f: &Fact) -> View {
    let style = if f.new {
        "display: flex; align-items: center; gap: 4px; padding: 4px 10px; border-radius: var(--m-radius-full); \
         background: var(--m-warning-soft); color: var(--m-warning); font-size: var(--m-font-size-sm); white-space: nowrap"
    } else {
        "display: flex; align-items: center; gap: 4px; padding: 4px 10px; border-radius: var(--m-radius-full); \
         background: var(--m-accent-soft); color: var(--m-accent-text); font-size: var(--m-font-size-sm); white-space: nowrap"
    };
    let pin: View = if f.pin {
        view! { <span style="font-family: 'Material Symbols Rounded'; font-size: 16px; line-height: 1">{icons::PIN}</span> }
    } else {
        View::Empty
    };
    let text = f.text.clone();
    view! { <span style={style}>{pin}<span>{text}</span></span> }
}

/// A quiet sentence where egui has `theme::note`.
fn note(text: &'static str) -> View {
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{text}</p>
    }
}

fn view_of(l: &Live) -> View {
    let frame = {
        let placement = l.placement.clone();
        move || placement.get().style()
    };
    let body = match &l.mode {
        Mode::Card => card_body(l),
        Mode::Compose(name) => compose_body(l, name),
    };
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too: `UiPointer` counts
    // only nodes that listen.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <div style="min-height: 0; overflow-y: auto; padding: 20px; display: flex; flex-direction: column; gap: 16px">
                {body}
            </div>
        </div>
    }
}

/// The card: who, the facts, ⋯'s Block and Report, the verb their relation
/// allows, and the greetings.
fn card_body(l: &Live) -> View {
    let l = l.clone();
    let body = move || -> View {
        let s = l.card.get();
        let Some(p) = s.person.clone() else {
            // Still asking, or nobody.
            let what: View = if s.looking {
                view! {
                    <div style="flex: 1; display: flex; align-items: center; gap: 12px; color: var(--m-text-dim)">
                        <Spinner size=Size::Sm />"Looking…"
                    </div>
                }
            } else {
                let msg = s
                    .error
                    .clone()
                    .unwrap_or_else(|| "Nobody to show here.".to_string());
                view! { <p style="flex: 1; margin: 0; color: var(--m-text-dim)">{msg}</p> }
            };
            return view! {
                <div style="display: flex; align-items: center; gap: 12px">
                    {what}
                    <CloseButton onclick={l.act(PersonAction::Close)} />
                </div>
            };
        };
        let facts: Vec<View> = p.facts.iter().map(fact).collect();
        let menu: View = if s.menu {
            view! {
                <div style="display: flex; gap: 8px">
                    {cell(view! { <Button size=Size::Sm variant=Variant::Danger icon={icon(icons::CLEAR)} onclick={l.act(PersonAction::Block)}>"Block"</Button> })}
                    {cell(view! { <Button size=Size::Sm icon={icon(icons::FLAG)} onclick={l.act(PersonAction::Report)}>"Report…"</Button> })}
                </div>
            }
        } else {
            View::Empty
        };
        let error: View = match &s.error {
            Some(e) => {
                let acts = l.acts.clone();
                let dismiss: Rc<dyn Fn()> = Rc::new(move || {
                    acts.borrow_mut().push(Act::Do(PersonAction::Close));
                });
                let e = e.clone();
                view! { <Alert tone=Tone::Danger ondismiss={Some(dismiss)}>{e}</Alert> }
            }
            None => View::Empty,
        };
        let verbs: View = match p.relation {
            Relation::Nobody if p.may_ask => view! {
                <div style="display: flex; gap: 8px">
                    {cell(view! { <Button size=Size::Sm variant=Variant::Primary icon={icon(icons::CHAT)} onclick={l.act(PersonAction::SayHi)}>"Say hi"</Button> })}
                    {cell(view! { <Button size=Size::Sm variant=Variant::Primary icon={icon(icons::ADD)} onclick={l.act(PersonAction::Add)}>"Add contact"</Button> })}
                </div>
            },
            Relation::Nobody => note("Not taking requests from you right now."),
            Relation::Contact => view! {
                <Button variant=Variant::Primary icon={icon(icons::CHAT)} onclick={l.act(PersonAction::Message)}>"Message"</Button>
            },
            Relation::RequestReceived => view! {
                <Button variant=Variant::Primary onclick={l.act(PersonAction::SeeRequest)}>"See their request"</Button>
            },
            Relation::RequestSent => note("They'll see your request in their tray."),
            Relation::Me => View::Empty,
        };
        // Greeting them (protocol 49): anybody standing near you, contact or
        // not. The sim checks the reach and the blocks.
        let greet: View = if p.can_greet {
            let buttons: Vec<View> = Greeting::ALL
                .iter()
                .map(|g| {
                    let label = g.label();
                    cell(view! {
                        <Button icon={icon(icons::HAND)} onclick={l.act(PersonAction::Greet(*g))} title="Offer it: within two paces, they take it with E or a tap">{label}</Button>
                    })
                })
                .collect();
            view! { <div style="display: flex; gap: 8px">{buttons}</div> }
        } else {
            View::Empty
        };
        let name = p.username.clone();
        let relation = relation_word(p.relation);
        view! {
            <div style="display: flex; align-items: center; gap: 16px">
                {initial(&p.username, 48.0)}
                <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                    <h1 style="margin: 0; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{name}</h1>
                    <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{relation}</span>
                </div>
                <Button variant=Variant::Ghost icon={icon(icons::MORE)} icon_only=true onclick={l.act(PersonAction::ToggleMenu)} title="Block or report" />
                <CloseButton onclick={l.act(PersonAction::Close)} />
            </div>
            <div style="display: flex; flex-wrap: wrap; gap: 8px">{facts}</div>
            {menu}
            {error}
            {verbs}
            {greet}
        }
    };
    view! { <div style="display: flex; flex-direction: column; gap: 16px">{body}</div> }
}

/// The first-message sheet: to whom, the line, quick lines, what they will
/// see, Cancel and Send request.
fn compose_body(l: &Live, name: &str) -> View {
    let title = format!("First message to {name}");
    let fine = hello_fine_print(name);
    let quick: Vec<View> = QUICK_HELLOS
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let q = *q;
            view! { <Button size=Size::Sm onclick={Some(l.on(Act::Quick(i)))}>{q}</Button> }
        })
        .collect();
    // Its own reactive node, so an error coming or going leaves the field
    // (and the keyboard on it) alone.
    let error = {
        let l = l.clone();
        move || -> View {
            match l.error.get() {
                Some(e) => {
                    let acts = l.acts.clone();
                    let dismiss: Rc<dyn Fn()> = Rc::new(move || {
                        acts.borrow_mut().push(Act::Do(PersonAction::Cancel));
                    });
                    view! { <Alert tone=Tone::Danger ondismiss={Some(dismiss)}>{e}</Alert> }
                }
                None => View::Empty,
            }
        }
    };
    let text = l.text.clone();
    let empty = l.empty.clone();
    view! {
        <div style="display: flex; align-items: center; gap: 16px">
            {initial(name, 40.0)}
            <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                <h1 style="margin: 0; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{title}</h1>
                <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">"Goes to their requests"</span>
            </div>
        </div>
        <FieldTextarea model={text} rows={3_u32} maxlength={MAX_HELLO_CHARS as u32} placeholder="Say hi" id="contacts-hello" />
        <div style="display: flex; flex-wrap: wrap; gap: 8px">{quick}</div>
        <div style="display: flex; align-items: flex-start; gap: 12px; padding: 12px 16px; border-radius: var(--m-radius); background: var(--m-surface-raised); color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
            {icon(icons::LOCK)}
            <span style="flex: 1; min-width: 0">{fine}</span>
        </div>
        {error}
        <div style="display: flex; align-items: center; gap: 8px">
            <Button variant=Variant::Ghost onclick={l.act(PersonAction::Cancel)}>"Cancel"</Button>
            <div style="flex: 1"></div>
            <Button variant=Variant::Primary icon={icon(icons::SEND)} disabled={empty} onclick={l.act(PersonAction::Send)} title="Write something first, then send">"Send request"</Button>
        </div>
    }
}
