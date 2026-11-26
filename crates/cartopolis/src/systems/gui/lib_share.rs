//! The Share sheet (bridge 3) drawn by the component library (`lib_host`):
//! "Share <name>", what it does, the recent chats and the communities to
//! send the place to, and Copy link.
//!
//! `bridges::drive_share_sheet` keeps every decision — which chats and
//! communities are offered and in what order, where the sheet stands,
//! whether Copy link shows its ✓, a tap off the sheet — and publishes it in
//! `Bridges::lib_view`. A tap here is a `bridges::ShareAct`, handed back in
//! `Bridges::lib_acts` and done by that system next frame.
//!
//! **Placement.** A card standing on the World | Community
//! switch, centred, as your face's sheet is on a phone (`ui::SheetSpot`).
//!
//! The ‹ Back chip of a Show in world is the chrome's
//! (`lib_chrome`'s search bar), not this module's.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::bridges::{Bridges, ShareAct, ShareView};
use crate::systems::lib_chats::{group, note};
use crate::systems::lib_place_card::icon;
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui::SheetSpot;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The sheet's name in the library's host.
const KEY: &str = "share_sheet";

type Acts = Rc<RefCell<Vec<ShareAct>>>;

#[derive(Clone)]
struct Live {
    dark: bool,
    /// The library's px per egui point: 1 on the Bevy backend, the UI scale
    /// as DOM (`lib_place_card::Placement::over_the_dock` says why).
    k: f32,
    view: Signal<ShareView>,
    acts: Acts,
}

impl Live {
    fn act(&self, a: ShareAct) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a.clone())) as Handler)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the Share sheet. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_share(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut bridges: ResMut<Bridges>,
    metrics: Res<UiMetrics>,
) {
    if !lib.share_sheet {
        return;
    }
    let dark = theme::p().ink == theme::DARK.ink;
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        metrics.ui_scale()
    } else {
        1.0
    };
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, for egui's system to do next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<ShareAct> = std::mem::take(&mut *live.acts.borrow_mut());
            if !acts.is_empty() {
                bridges.lib_acts.extend(acts);
            }
        }
        // Put away, or a new theme or scale: down (to mount again).
        let stale = m
            .as_ref()
            .is_some_and(|l| bridges.lib_view.is_none() || l.dark != dark || l.k != k);
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let Some(view) = bridges.lib_view.clone() else {
            return;
        };
        match m.as_ref() {
            None => {
                let live = Live {
                    dark,
                    k,
                    view: signal(view),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    view_of(&live),
                    dark,
                    theme::touch_layout(),
                );
                write_html(&live);
                *m = Some(live);
            }
            Some(live) => {
                if live.view.peek(|now| *now != view) {
                    live.view.set(view);
                    write_html(live);
                }
            }
        }
    });
}

/// `CARTO_UI_LIB_HTML=<path>`: the sheet as HTML, for the browser beside a
/// capture.
fn write_html(l: &Live) {
    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
        if let Err(e) = std::fs::write(&path, html(l)) {
            warn!(?path, error = %e, "lib share: could not write the HTML");
        }
    }
}

/// The sheet's HTML from the library's string renderer.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view_of(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

/// The card's frame at its spot: the library's card surface.
fn frame_style(spot: SheetSpot, k: f32) -> String {
    let (at, width, max_height) = match spot {
        SheetSpot::Bottom {
            left,
            bottom,
            width,
            max_height,
        } => (
            format!("left: {:.0}px; bottom: {:.0}px", left * k, bottom * k),
            width,
            max_height,
        ),
        SheetSpot::Hang {
            right,
            top,
            width,
            max_height,
        } => (
            format!("right: {:.0}px; top: {:.0}px", right * k, top * k),
            width,
            max_height,
        ),
    };
    format!(
        "position: absolute; {at}; width: {:.0}px; max-height: {:.0}px; box-sizing: border-box; \
         display: flex; flex-direction: column; gap: 12px; padding: 16px; \
         background: var(--m-surface); border: 1px solid var(--m-border); \
         border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg); overflow: hidden",
        width * k,
        max_height * k
    )
}

/// The sheet: the title and ✕, what it does, the places to send it (which
/// scroll), and Copy link at the foot.
fn view_of(l: &Live) -> View {
    let frame = {
        let (v, k) = (l.view.clone(), l.k);
        move || frame_style(v.get().spot, k)
    };
    let head = {
        let l = l.clone();
        move || -> View {
            let title = format!("Share {}", l.view.get().name);
            view! {
                <div style="display: flex; align-items: center; gap: 12px">
                    <h1 style="flex: 1; min-width: 0; margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong); white-space: nowrap; overflow: hidden; text-overflow: ellipsis">{title}</h1>
                    <CloseButton onclick={l.act(ShareAct::Close)} />
                </div>
            }
        }
    };
    let places = {
        let l = l.clone();
        move || -> View { targets(&l, &l.view.get()) }
    };
    let copy = {
        let l = l.clone();
        move || -> View {
            let (glyph, word) = if l.view.get().copied {
                (icons::CHECK, "Link copied")
            } else {
                (icons::COPY, "Copy link")
            };
            view! {
                <div style="display: flex; flex-direction: column">
                    <Button icon={icon(glyph)} onclick={l.act(ShareAct::Copy)}>{word}</Button>
                </div>
            }
        }
    };
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too, and a tap there is not
    // a tap off it.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <div style="display: flex; flex-direction: column; gap: 4px">
                {head}
                <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">"Sends the place to a chat or community"</span>
            </div>
            <div style="min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 8px">
                {places}
            </div>
            {copy}
        </div>
    }
}

/// Where it can go: a word when nowhere, else "Send to" over the chats and
/// the communities, in egui's order.
fn targets(l: &Live, v: &ShareView) -> View {
    if !v.signed_in {
        return note("Sign in to send it to people and communities.");
    }
    if v.chats.is_empty() && v.communities.is_empty() {
        return note("No chats or communities to send it to yet.");
    }
    let mut rows: Vec<View> = v
        .chats
        .iter()
        .map(|(key, name, topic)| {
            let sub = if *topic { "Topic" } else { "Chat" };
            let name = name.clone();
            view! {
                <ListItem icon={icon(icons::CHAT)} sub={sub.into_view()} chevron=true onclick={l.act(ShareAct::Send(key.clone()))}>{name}</ListItem>
            }
        })
        .collect();
    rows.extend(v.communities.iter().map(|(id, name)| {
        let name = name.clone();
        view! {
            <ListItem icon={icon(icons::GROUP)} sub={"Community · as a post".into_view()} chevron=true onclick={l.act(ShareAct::Post(*id))}>{name}</ListItem>
        }
    }));
    view! {
        {group("Send to")}
        <List>{rows}</List>
    }
}
