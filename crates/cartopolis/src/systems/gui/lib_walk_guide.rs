//! The walking chrome drawn by the component library (`lib_host`), beside
//! egui's (`walk_guide::render_walk_guide`): the next turn at the top left,
//! "Walking you there" under it while the avatar walks itself, and the bar
//! above the dock with what is left and what to do about it.
//!
//! On with `CARTO_UI_LIB=walk_guide` (or `1`), on the web `?ui=lib`. egui's
//! system steps aside (`WalkGuide::lib`) but still works out what shows —
//! the turn only with no panel open and before arriving, the Skip list's
//! targets from the route's geometry — and publishes it in
//! `WalkGuide::lib_view`. A tap here goes back in `WalkGuide::lib_acts` and
//! is done by `WalkGuide::apply`, the code egui's buttons call.
//!
//! **Placement.** As egui's: the turn card `walk_guide::TURN_INSET` in from
//! the left and `OSD_MARGIN` down, no wider than the band between the two
//! columns of round buttons; the chip centred `walk_guide::AUTO_TOP` down;
//! the bar centred over the bottom edge, `BAR_GAP` above the dock
//! (`dock::reserved_bottom`), as wide as 420 or the screen less its margins.
//! Each frame is egui's `overlay_frame`: 14 × 12 inside, a 1 px rim.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::lib_place_card::icon;
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme::tokens::hex;
use crate::systems::ui_input::UiMetrics;
use crate::systems::walk_guide::{
    WalkAct, WalkGuide, WalkView, AUTO_TOP, BAR_GAP, BAR_WIDTH, TURN_INSET, TURN_WIDTH,
};
use crate::systems::{icons, theme};

/// The walking chrome's name in the library's host.
const KEY: &str = "walk_guide";

/// egui's `overlay_frame`, as a style: the surface, its rim, 14 × 12 inside.
/// Border-box, so a width below is the frame's whole width.
const FRAME: &str = "box-sizing: border-box; padding: 12px 14px; \
     background: var(--m-surface); border: 1px solid var(--m-border); \
     border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow)";

/// What [`FRAME`] adds across to its content, px: both paddings and rims.
const FRAME_X: f32 = 2.0 * 14.0 + 2.0;

/// The viewport the chrome is placed in, and the dock's top.
#[derive(Clone, Copy, PartialEq, Default)]
struct Placement {
    /// The viewport's width, egui points.
    w: f32,
    /// The bar's bottom edge above the window's, egui points.
    bottom: f32,
    /// The library's px per egui point: 1 on the Bevy backend, the UI scale
    /// as DOM (`lib_place_card::Placement::over_the_dock` says why).
    k: f32,
}

impl Placement {
    fn of(w: f32, ui_scale: f32) -> Self {
        let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            ui_scale
        } else {
            1.0
        };
        Placement {
            w,
            bottom: crate::systems::dock::reserved_bottom() + BAR_GAP,
            k,
        }
    }

    /// The next-turn card: top left, between the columns of round buttons.
    fn turn(&self) -> String {
        let between = self.w - 2.0 * TURN_INSET;
        let max = (TURN_WIDTH.min(between - 24.0) + FRAME_X).max(0.0);
        format!(
            "position: absolute; left: {:.0}px; top: {:.0}px; max-width: {:.0}px; {}; \
             display: flex; align-items: center; gap: 10px",
            TURN_INSET * self.k,
            theme::OSD_MARGIN * self.k,
            max * self.k,
            FRAME
        )
    }

    /// The band the "Walking you there" chip is centred in: the window's
    /// width, listening nowhere.
    fn chip_band(&self) -> String {
        format!(
            "position: absolute; left: 0; top: {:.0}px; width: {:.0}px; \
             display: flex; justify-content: center; pointer-events: none",
            AUTO_TOP * self.k,
            self.w * self.k
        )
    }

    /// The bar: centred over the bottom edge, above the dock.
    fn bar(&self) -> String {
        let width = (self.w - 2.0 * theme::OSD_MARGIN).min(BAR_WIDTH);
        // egui sets the content `width - 24` wide; the frame adds its own.
        let total = (width - 24.0 + FRAME_X).max(0.0);
        format!(
            "position: absolute; left: {:.0}px; bottom: {:.0}px; width: {:.0}px; {}; \
             display: flex; flex-direction: column; gap: 6px",
            ((self.w - total) / 2.0).max(0.0) * self.k,
            self.bottom * self.k,
            total * self.k,
            FRAME
        )
    }
}

type Acts = Rc<RefCell<Vec<WalkAct>>>;

#[derive(Clone)]
struct Live {
    view: Signal<WalkView>,
    placement: Signal<Placement>,
    acts: Acts,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// A tap that hands `a` back.
fn on(acts: &Acts, a: WalkAct) -> Handler {
    let acts = acts.clone();
    Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler
}

/// Mounts, bridges and unmounts the walking chrome. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_walk_guide(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut guide: ResMut<WalkGuide>,
    metrics: Res<UiMetrics>,
) {
    if !lib.walk_guide {
        return;
    }
    if !guide.lib {
        guide.lib = true;
    }
    let placement = Placement::of(metrics.viewport_pts().x, metrics.ui_scale());
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, for egui's system to do next frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<WalkAct> = std::mem::take(&mut *live.acts.borrow_mut());
            if !acts.is_empty() {
                guide.lib_acts.extend(acts);
            }
        }
        let Some(view) = guide.lib_view.clone() else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        };
        match m.as_mut() {
            None => {
                let live = Live {
                    view: signal(view),
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
                        warn!(?path, error = %e, "lib walk guide: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                if live.view.peek(|now| *now != view) {
                    live.view.set(view);
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

/// The root: the whole window, listening nowhere, the three pieces placed in
/// it. Over the panels, as egui's are (`Order::Foreground`).
fn view_of(l: &Live) -> View {
    let turn = {
        let (view, placement) = (l.view.clone(), l.placement.clone());
        move || -> View { turn_view(&view.get(), &placement.get()) }
    };
    let chip = {
        let (view, placement) = (l.view.clone(), l.placement.clone());
        move || -> View {
            if !view.get().auto_chip {
                return View::Empty;
            }
            chip_view(&placement.get())
        }
    };
    let bar = {
        let (view, placement, acts) = (l.view.clone(), l.placement.clone(), l.acts.clone());
        move || -> View { bar_view(&view.get(), &placement.get(), &acts) }
    };
    view! {
        <div style="position: absolute; left: 0; top: 0; right: 0; bottom: 0; pointer-events: none">
            {turn}
            {chip}
            {bar}
        </div>
    }
}

/// The next turn: the manoeuvre on the brand's plate, how far, and where.
fn turn_view(v: &WalkView, p: &Placement) -> View {
    let Some(t) = v.turn.clone() else {
        return View::Empty;
    };
    let frame = format!("{}; pointer-events: auto", p.turn());
    // egui paints the plate in the brand's orange with a white glyph.
    let plate = format!(
        "flex: none; width: 38px; height: 38px; display: flex; align-items: center; \
         justify-content: center; border-radius: 9px; background: {}; color: {}; \
         font-family: 'Material Symbols Rounded'; font-size: 24px; line-height: 1",
        hex(theme::BRAND),
        hex(theme::Rgba8::WHITE)
    );
    let count = format!(
        "font-size: {}px; font-weight: 600; color: var(--m-text-strong); line-height: 1.2",
        theme::SIZE_HEADING
    );
    let street = format!(
        "font-size: {}px; color: var(--m-text-dim); line-height: 1.3",
        theme::SIZE_SUB
    );
    let glyph = t.glyph;
    let countdown = t.countdown;
    let line = t.street;
    // The card's own node listens (a click that does nothing), as egui's
    // area takes the pointer over it.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <span style={plate}>{glyph}</span>
            <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                <span style={count}>{countdown}</span>
                <span style={street}>{line}</span>
            </div>
        </div>
    }
}

/// "Walking you there · any key takes over", while the avatar walks itself.
fn chip_view(p: &Placement) -> View {
    let band = p.chip_band();
    let frame = format!(
        "{}; pointer-events: auto; color: var(--m-text); font-size: {}px; white-space: nowrap",
        FRAME,
        theme::SIZE_SUB
    );
    view! {
        <div style={band}>
            <div style={frame} on:click={|_: &Event| {}}>"Walking you there · any key takes over"</div>
        </div>
    }
}

/// The bar: the Skip list when open, then what is left, where to, a note,
/// and Skip, Auto or Pause, and Stop or Done.
fn bar_view(v: &WalkView, p: &Placement, acts: &Acts) -> View {
    let frame = format!("{}; pointer-events: auto", p.bar());
    let skip: View = match &v.skip {
        Some(targets) => {
            let heading = format!(
                "color: var(--m-text-dim); font-size: {}px; padding-bottom: 2px",
                theme::SIZE_SUB
            );
            let row = format!(
                "min-height: 36px; display: flex; align-items: center; padding: 0 10px; \
                 border-radius: var(--m-radius); color: var(--m-text); font-size: {}px; \
                 white-space: nowrap; overflow: hidden; cursor: pointer",
                theme::SIZE_BODY
            );
            let rows: Vec<View> = targets
                .iter()
                .map(|(line, along)| {
                    let tap = on(acts, WalkAct::SkipTo(*along));
                    let line = line.clone();
                    let style = row.clone();
                    view! {
                        <div style={style} on:click={move |e: &Event| tap(e)}>{line}</div>
                    }
                })
                .collect();
            let list = View::Fragment(rows);
            view! {
                <div style="display: flex; flex-direction: column; gap: 2px">
                    <span style={heading}>"Skip ahead to…"</span>
                    {list}
                </div>
            }
        }
        None => View::Empty,
    };
    let headline: View = if v.arrived {
        let style = format!(
            "font-size: {}px; font-weight: 600; color: var(--m-text-strong)",
            theme::SIZE_TITLE
        );
        view! { <span style={style}>"You've arrived"</span> }
    } else {
        let style = format!(
            "font-size: {}px; font-weight: 600; color: var(--m-text-strong)",
            theme::SIZE_VALUE
        );
        let left = v.left.clone();
        view! { <span style={style}>{left}</span> }
    };
    let sub = format!(
        "font-size: {}px; color: var(--m-text-dim); white-space: nowrap; overflow: hidden; text-overflow: ellipsis",
        theme::SIZE_SUB
    );
    let destination = v.destination.clone();
    let note: View = match v.note {
        Some(note) => {
            let style = format!("font-size: {}px; color: var(--m-warning)", theme::SIZE_SUB);
            view! { <span style={style}>{note}</span> }
        }
        None => View::Empty,
    };
    // Left to right as egui's right-to-left row puts them: Skip, Auto or
    // Pause, then Stop (Done once arrived, alone).
    let walking: View = if v.arrived {
        View::Empty
    } else {
        let auto: View = if v.auto {
            view! {
                <Button variant=Variant::Ghost size=Size::Sm onclick={Some(on(acts, WalkAct::ToggleAuto))} title="Walk me there: the avatar follows the route">"Pause"</Button>
            }
        } else {
            view! {
                <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::PLAY)} onclick={Some(on(acts, WalkAct::ToggleAuto))} title="Walk me there: the avatar follows the route">"Auto"</Button>
            }
        };
        view! {
            <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::FORWARD)} onclick={Some(on(acts, WalkAct::ToggleSkip))} title="Jump ahead along the route">"Skip"</Button>
            {auto}
        }
    };
    let stop = if v.arrived { "Done" } else { "Stop" };
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            {skip}
            // The words over the buttons, not beside them: at a phone's
            // width the row left "18 min · 1.4 km left" a column one word
            // wide (egui's row overlaps there too).
            <div style="min-width: 0; display: flex; flex-direction: column; gap: 2px">
                {headline}
                <span style={sub}>{destination}</span>
                {note}
            </div>
            <div style="display: flex; align-items: center; justify-content: flex-end; gap: 4px">
                {walking}
                <Button variant=Variant::Ghost size=Size::Sm onclick={Some(on(acts, WalkAct::Stop))}>{stop}</Button>
            </div>
        </div>
    }
}
