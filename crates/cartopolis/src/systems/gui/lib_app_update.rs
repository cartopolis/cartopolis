//! The app's own updates drawn by the component library (`lib_host`),
//! beside egui's (`app_update::render_app_update`): the pill at the top
//! when a newer build is downloaded, and the card over everything when the
//! server refuses this build as too old.
//!
//! On with `CARTO_UI_LIB=app_update` (or `1`). egui's system steps aside
//! (`AppUpdate::lib`) but keeps asking the updater and deciding what shows,
//! published as `AppUpdate::lib_view` — the same `UpdateView` its own
//! drawing reads. A tap here goes back in `AppUpdate::lib_action` and is
//! done by `AppUpdate::act`, the code egui's buttons call.
//!
//! **Placement.** The pill is centred under the top chrome
//! (`dock::reserved_top`), as egui's. The too-old card is a full-window
//! veil that listens (so no tap reaches the map or a panel) with a column
//! of at most [`CARD_WIDTH`] centred in it.
//!
//! **What differs from egui's.** Nothing but the look.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::app_update::{too_old_text, AppUpdate, TooOld, UpdateAction, UpdateView};
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The screen's name in the library's host.
const KEY: &str = "app_update";

/// Width of the pill, px, before the screen narrows it.
const PILL_WIDTH: f32 = 360.0;

/// Width of the too-old card's column, px, before the screen narrows it.
const CARD_WIDTH: f32 = 380.0;

/// Kept clear around the too-old card, px, per side.
const EDGE: f32 = 20.0;

/// Where the pill stands and how wide the card is, px in the library's root.
#[derive(Clone, Copy, PartialEq, Default)]
struct Placement {
    pill_left: f32,
    pill_top: f32,
    pill_width: f32,
    card_width: f32,
}

impl Placement {
    /// For a viewport `w` egui points wide; as DOM the page's px are
    /// `ui_scale` times larger (`lib_place_card::Placement`).
    fn of(w: f32, ui_scale: f32) -> Self {
        let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            ui_scale
        } else {
            1.0
        };
        let pill = PILL_WIDTH.min(w - 2.0 * theme::OSD_MARGIN).max(0.0);
        Placement {
            pill_left: ((w - pill) / 2.0).max(0.0) * k,
            pill_top: crate::systems::dock::reserved_top() * k,
            pill_width: pill * k,
            card_width: CARD_WIDTH.min(w - 2.0 * EDGE).max(160.0) * k,
        }
    }
}

/// Whether the view is the pill or the card: another kind mounts again.
fn kind(v: &UpdateView) -> u8 {
    match v {
        UpdateView::None => 0,
        UpdateView::Pill { .. } => 1,
        UpdateView::TooOld(_) => 2,
    }
}

#[derive(Clone)]
struct Live {
    view: Signal<UpdateView>,
    placement: Signal<Placement>,
    acts: Rc<RefCell<Vec<UpdateAction>>>,
}

impl Live {
    fn act(&self, a: UpdateAction) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the pill and the card. `PostUpdate`,
/// before `DriveUi`, as every library screen.
pub fn drive_lib_app_update(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut state: ResMut<AppUpdate>,
    metrics: Res<UiMetrics>,
) {
    if !lib.app_update {
        return;
    }
    if !state.lib {
        state.lib = true;
    }
    let placement = Placement::of(metrics.viewport_pts().x, metrics.ui_scale());
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The tap of the frame, for egui's system to do next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<UpdateAction> = std::mem::take(&mut *live.acts.borrow_mut());
            if let Some(a) = acts.last() {
                state.lib_action = Some(*a);
            }
        }
        let view = state.lib_view.clone();
        // Nothing to show, or the pill turned into the card.
        let stale = m
            .as_ref()
            .is_some_and(|live| kind(&live.view.get_untracked()) != kind(&view));
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        if view == UpdateView::None {
            return;
        }
        match m.as_mut() {
            None => {
                let live = Live {
                    view: signal(view.clone()),
                    placement: signal(placement),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                let p = theme::p();
                let tree = if kind(&view) == 1 {
                    pill(&live)
                } else {
                    card(&live)
                };
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    tree,
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                *m = Some(live);
            }
            Some(live) => {
                if live.view.get_untracked() != view {
                    live.view.set(view);
                }
                if live.placement.get_untracked() != placement {
                    live.placement.set(placement);
                }
            }
        }
    });
}

/// An icon glyph in the icon face, at `size` px.
fn glyph(glyph: &'static str, size: u32) -> View {
    let style =
        format!("font-family: 'Material Symbols Rounded'; font-size: {size}px; line-height: 1");
    view! { <span style={style}>{glyph}</span> }
}

/// "Update ready", with Install and ✕ — and once tapped, how far it is.
fn pill(l: &Live) -> View {
    let frame = {
        let placement = l.placement.clone();
        move || {
            let p = placement.get();
            format!(
                "position: absolute; left: {:.0}px; top: {:.0}px; width: {:.0}px; \
                 display: flex; align-items: center; gap: 12px; padding: 12px 12px 12px 20px; \
                 background: var(--m-surface); border: 1px solid var(--m-border); \
                 border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
                p.pill_left, p.pill_top, p.pill_width
            )
        }
    };
    let body = {
        let l = l.clone();
        move || -> View {
            let UpdateView::Pill { title, line, ready } = l.view.get() else {
                return View::Empty;
            };
            let end: View = if ready {
                view! {
                    <Button variant=Variant::Primary size=Size::Sm onclick={l.act(UpdateAction::Install)}>"Install"</Button>
                    <CloseButton onclick={l.act(UpdateAction::Dismiss)} label="Not now" />
                }
            } else {
                view! { <Spinner size=Size::Sm /> }
            };
            view! {
                <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 4px">
                    <span style="font-weight: 600; color: var(--m-text-strong)">{title}</span>
                    <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{line}</span>
                </div>
                {end}
            }
        }
    };
    // The pill's own node listens, so a tap on its padding stays off the map.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <span style="color: var(--m-accent)">{glyph(icons::DOWNLOAD, 24)}</span>
            {body}
        </div>
    }
}

/// "Update Cartopolis to continue": the map dimmed, nothing else answers.
fn card(l: &Live) -> View {
    let column = {
        let placement = l.placement.clone();
        move || {
            format!(
                "flex: none; width: {:.0}px; display: flex; flex-direction: column; \
                 align-items: center; gap: 16px; padding: 20px; text-align: center; \
                 background: var(--m-surface); border: 1px solid var(--m-border); \
                 border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
                placement.get().card_width
            )
        }
    };
    let body = {
        let l = l.clone();
        move || -> View {
            let UpdateView::TooOld(t) = l.view.get() else {
                return View::Empty;
            };
            let (heading, text) = too_old_text(&t);
            let progress: View = match &t {
                TooOld::Fetching { fraction, line } => {
                    let line = line.clone();
                    let pct = f64::from(*fraction) * 100.0;
                    view! {
                        <div style="align-self: stretch; display: flex; flex-direction: column; gap: 8px">
                            <Progress value={Some(pct)} />
                            <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{line}</span>
                        </div>
                    }
                }
                _ => View::Empty,
            };
            let offer: View = match &t {
                TooOld::Offer { failed } => {
                    let error: View = match failed {
                        Some(why) => {
                            let why = format!("Did not work: {why}");
                            view! { <div style="align-self: stretch; text-align: left"><Alert tone=Tone::Danger>{why}</Alert></div> }
                        }
                        None => View::Empty,
                    };
                    let label = if failed.is_some() {
                        "Try again"
                    } else {
                        "Update now"
                    };
                    view! {
                        {error}
                        <div style="align-self: stretch; display: flex; flex-direction: column">
                            <Button variant=Variant::Primary size=Size::Lg onclick={l.act(UpdateAction::Install)}>{label}</Button>
                        </div>
                    }
                }
                _ => View::Empty,
            };
            view! {
                <h1 style="margin: 0; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{heading}</h1>
                {progress}
                <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{text}</p>
                {offer}
            }
        }
    };
    // The veil listens (a tap that does nothing), so no tap reaches the map
    // or a panel beneath it: nothing there works until the update is in.
    view! {
        <div style="position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; align-items: center; justify-content: center; padding: 20px; background: var(--m-backdrop)" on:click={|_: &Event| {}}>
            <div style={column}>
                <span style="color: var(--m-accent)">{glyph(icons::DOWNLOAD, 40)}</span>
                {body}
            </div>
        </div>
    }
}
