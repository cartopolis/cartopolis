//! The calendar's reminder drawn by the component library (`lib_host`),
//! beside egui's (`calendar::render_reminder`): time to leave for a meet-up,
//! or a meeting about to start, over whatever is open.
//!
//! On with `CARTO_UI_LIB=reminder` (or `1`), on the web `?ui=lib`. egui's
//! system steps aside (`Calendar::lib`) but still works out when a reminder
//! is due and when it goes; a tap here is handed back in
//! `Calendar::lib_action` and done by `calendar::reminder_act`, the code
//! egui's buttons call.
//!
//! **Placement.** As egui's: centred over the top edge, 64 px down, as wide
//! as 420 px or the screen less its margins.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::calendar::{Calendar, Reminder, ReminderAction};
use crate::systems::lib_place_card::icon;
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The reminder's name in the library's host.
const KEY: &str = "reminder";

/// How far below the top edge it stands, px: egui's figure.
const TOP: f32 = 64.0;

/// Its widest, px.
const WIDTH: f32 = 420.0;

/// Where the reminder stands, px in the library's root.
#[derive(Clone, Copy, PartialEq, Default)]
struct Placement {
    left: f32,
    top: f32,
    width: f32,
}

impl Placement {
    fn of(w: f32, ui_scale: f32) -> Self {
        let width = WIDTH.min(w - 2.0 * theme::OSD_MARGIN).max(0.0);
        let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            ui_scale
        } else {
            1.0
        };
        Placement {
            left: ((w - width) / 2.0).max(0.0) * k,
            top: TOP * k,
            width: width * k,
        }
    }

    fn style(&self) -> String {
        format!(
            "position: absolute; left: {:.0}px; top: {:.0}px; width: {:.0}px; \
             display: flex; flex-direction: column; gap: 16px; padding: 20px; \
             background: var(--m-surface); border: 1px solid var(--m-border); \
             border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
            self.left, self.top, self.width
        )
    }
}

#[derive(Clone)]
struct Live {
    /// The reminder mounted: another one mounts it again.
    reminder: Reminder,
    placement: Signal<Placement>,
    acts: Rc<RefCell<Vec<ReminderAction>>>,
}

impl Live {
    fn act(&self, a: ReminderAction) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the reminder. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_reminder(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut cal: ResMut<Calendar>,
    metrics: Res<UiMetrics>,
) {
    if !lib.reminder {
        return;
    }
    if !cal.lib {
        cal.lib = true;
    }
    let placement = Placement::of(metrics.viewport_pts().x, metrics.ui_scale());
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The tap of the frame, for egui's system to do.
        if let Some(live) = m.as_ref() {
            let acts: Vec<ReminderAction> = std::mem::take(&mut *live.acts.borrow_mut());
            if let Some(a) = acts.last() {
                cal.lib_action = Some(*a);
            }
        }
        let stale = m
            .as_ref()
            .is_some_and(|live| cal.reminder.as_ref() != Some(&live.reminder));
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let Some(reminder) = cal.reminder.clone() else {
            return;
        };
        match m.as_mut() {
            None => {
                let live = Live {
                    reminder,
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
                        warn!(?path, error = %e, "lib reminder: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                if live.placement.get_untracked() != placement {
                    live.placement.set(placement);
                }
            }
        }
    });
}

/// The demo reminder's view (`calendar::seed_demo`), for a test that reads
/// a real screen through the library.
#[cfg(test)]
pub(crate) fn demo_view() -> View {
    let mut cal = Calendar::default();
    crate::systems::calendar::seed_demo(&mut cal, None, false, true);
    let reminder = cal.reminder.expect("the demo has a meet-up");
    view_of(&Live {
        reminder,
        placement: signal(Placement::of(800.0, 1.0)),
        acts: Rc::new(RefCell::new(Vec::new())),
    })
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

/// The reminder: the icon, what and when, ✕; Open chat and the way there.
fn view_of(l: &Live) -> View {
    let r = &l.reminder;
    let frame = {
        let placement = l.placement.clone();
        move || placement.get().style()
    };
    let title = r.title.clone();
    let line = r.line.clone();
    let go = r.go_label();
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <div style="display: flex; align-items: flex-start; gap: 16px">
                <span style="flex: none; padding-top: 2px; color: var(--m-accent)">{icon(r.icon())}</span>
                <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 4px">
                    <h1 style="margin: 0; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{title}</h1>
                    <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{line}</span>
                </div>
                <CloseButton onclick={l.act(ReminderAction::Close)} />
            </div>
            <div style="display: flex; flex-wrap: wrap; gap: 8px">
                <Button size=Size::Sm icon={icon(icons::CHAT)} onclick={l.act(ReminderAction::OpenChat)}>"Open chat"</Button>
                <Button size=Size::Sm variant=Variant::Primary icon={icon(r.icon())} onclick={l.act(ReminderAction::Go)}>{go}</Button>
            </div>
        </div>
    }
}
