//! The first-launch card drawn by the component library (`lib_host`),
//! beside egui's (`welcome::render_welcome`): what signing in is for, the
//! two ways in, and a way to just look at the map.
//!
//! On with `CARTO_UI_LIB=welcome` (or `1`). egui's system steps aside
//! (`Welcome::lib`) but keeps deciding whether the card shows — the saved
//! answer, the capture override, Community chosen signed out, and
//! everything that owns the bottom of the screen — and publishes that in
//! `Welcome::lib_view`. An answer tapped here goes back in
//! `Welcome::lib_action` and is given by `welcome::answer`, the code egui's
//! buttons call, so the two editions cannot drift apart.
//!
//! **Placement.** The place card's (`lib_place_card::Placement`): centred
//! over the bottom edge, above the dock. The card's own node listens, so a
//! tap on its padding stays off the world.
//!
//! **What differs from egui's.** Nothing but the look: wider, with 20 px
//! edges, as the library's cards are.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::lib_place_card::{cell, Placement};
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;
use crate::systems::welcome::{Welcome, WelcomeAction};

/// The card's name in the library's host.
const KEY: &str = "welcome";

#[derive(Clone)]
struct Live {
    /// The server signs people up on its web page.
    register: Signal<bool>,
    placement: Signal<Placement>,
    acts: Rc<RefCell<Vec<WelcomeAction>>>,
}

impl Live {
    fn act(&self, a: WelcomeAction) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the welcome card. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_welcome(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut welcome: ResMut<Welcome>,
    metrics: Res<UiMetrics>,
) {
    if !lib.welcome {
        return;
    }
    if !welcome.lib {
        welcome.lib = true;
    }
    let size = metrics.viewport_pts();
    let placement = Placement::over_the_dock(size.x, size.y, metrics.ui_scale());
    let view = welcome.lib_view;
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The answer of the frame, for egui's system to give next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<WelcomeAction> = std::mem::take(&mut *live.acts.borrow_mut());
            if let Some(a) = acts.last() {
                welcome.lib_action = Some(*a);
            }
        }
        // Answered: gone now, rather than a frame later when egui's system
        // has given the answer.
        let open = view.shown && welcome.lib_action.is_none();
        if !open {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        }
        match m.as_mut() {
            None => {
                let live = Live {
                    register: signal(view.register),
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
                        warn!(?path, error = %e, "lib welcome: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                if live.register.get_untracked() != view.register {
                    live.register.set(view.register);
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

/// The card: the heading, what signing in is for, the two ways in, and
/// the map.
fn view_of(l: &Live) -> View {
    let frame = {
        let placement = l.placement.clone();
        move || placement.get().style()
    };
    let ways_in = {
        let l = l.clone();
        move || -> View {
            let sign_in = cell(view! {
                <Button variant=Variant::Primary onclick={l.act(WelcomeAction::SignIn)}>"Sign in"</Button>
            });
            let register = if l.register.get() {
                cell(view! {
                    <Button variant=Variant::Secondary onclick={l.act(WelcomeAction::CreateAccount)}>"Create account"</Button>
                })
            } else {
                View::Empty
            };
            view! {
                <div style="display: flex; gap: 8px">{sign_in}{register}</div>
            }
        }
    };
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <div style="overflow-y: auto; padding: 20px; display: flex; flex-direction: column; gap: 16px">
                <div style="display: flex; flex-direction: column; gap: 8px">
                    <h1 style="margin: 0; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">"Cartopolis is better with people"</h1>
                    <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">"Sign in to see who is around, chat, and join the communities near you."</p>
                </div>
                {ways_in}
                <div style="display: flex; flex-direction: column">
                    <Button variant=Variant::Ghost onclick={l.act(WelcomeAction::NotNow)}>"Not now, just the map"</Button>
                </div>
            </div>
        </div>
    }
}
