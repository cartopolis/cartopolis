//! The navigation chrome drawn by the component library (`lib_host`), beside
//! egui's (`driver::render_driver_hud`): the turn card at the top left, the
//! trip bar along the bottom, the speed and the limit over the bar's left end,
//! Overview / Re-centre over its right, and the GPS chip between them.
//!
//! On with `CARTO_UI_LIB=driver` (or `1`), on the web `?ui=lib`. egui's
//! system steps aside (`DriverState::lib`) but still works out what shows —
//! the card's state and colour, the bar standing down behind a docked panel,
//! the chip only when there is something to say — and publishes it in
//! `DriverState::lib_view`. A tap here goes back in `DriverState::lib_acts`
//! and is done by `DriverState::apply`, the code egui's buttons call.
//!
//! **Placement.** As egui's: the card `CARD_MARGIN` in from the top left,
//! `card_width` × `CARD_HEIGHT`, 10 inside; the bar centred
//! `driver::bar_bottom` over the bottom edge, `bar_width` × `BAR_HEIGHT`, 8
//! inside; the speed and the view buttons with their bottom edge at
//! `driver::above_bar`, in from the screen's edges where the bar's ends are;
//! the chip centred between them. egui works the widths out
//! (`driver_layout`) and publishes them in `DriverView::place`.
//!
//! **Drawn from boxes.** What egui paints: the plate a rounded box, the
//! approach bar and the trip's progress hairline a track with a fill box on
//! it, the speed disc and the speed-limit sign circles by a full radius — the
//! sign a white disc with a thick red border.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::driver::{
    above_bar, bar_bottom, DriverAct, DriverBar, DriverCard, DriverPlace, DriverSpeed, DriverState,
    BAR_HEIGHT, CARD_HEIGHT, CARD_MARGIN, ROUTE_BLUE, SIGN_RED, SIGN_WHITE, SPEED_DISC, TILE,
};
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme::tokens::{alpha, hex};
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The navigation chrome's name in the library's host.
const KEY: &str = "driver";

/// egui's `overlay_frame` without its margin, as a style: the surface and
/// its rim. Border-box, so a width beside it is the frame's whole width.
const SURFACE: &str = "box-sizing: border-box; background: var(--m-surface); \
     border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); \
     box-shadow: var(--m-shadow)";

/// A round button's diameter (`theme::round_button`).
const DISC: f32 = theme::ROUND_BUTTON_SIZE;

/// egui's `item_spacing.x` (`theme::style`): the gap in the bar's row and
/// between the view buttons.
const GAP: f32 = 8.0;

/// The progress hairline's inset from the bar's ends, and its thickness.
const LINE_INSET: f32 = 14.0;
const LINE_W: f32 = 3.0;

/// The speed-limit sign's radius (`driver::speed_limit_sign`) and the width
/// of its red ring.
const SIGN_R: f32 = 13.0;
const SIGN_RING: f32 = 3.8;

type Acts = Rc<RefCell<Vec<DriverAct>>>;

/// What is mounted: one signal per piece, and what it was mounted for.
struct Live {
    dark: bool,
    /// The library's px per egui point: 1 on the Bevy backend, the UI scale
    /// as DOM (`lib_place_card::Placement::over_the_dock` says why).
    k: f32,
    acts: Acts,
    place: Signal<DriverPlace>,
    card: Signal<DriverCard>,
    bar: Signal<Option<DriverBar>>,
    speed: Signal<Option<DriverSpeed>>,
    /// (peeking, overview), while the bottom edge shows.
    buttons: Signal<Option<(bool, bool)>>,
    chip: Signal<Option<String>>,
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

/// Mounts, bridges and unmounts the navigation chrome. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_driver(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut driver: ResMut<DriverState>,
    metrics: Res<UiMetrics>,
) {
    if !lib.driver {
        return;
    }
    if !driver.lib {
        driver.lib = true;
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
            let acts: Vec<DriverAct> = std::mem::take(&mut *live.acts.borrow_mut());
            if !acts.is_empty() {
                driver.lib_acts.extend(acts);
            }
        }
        let Some(view) = driver.lib_view.as_ref() else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        };
        // The light follows the sun (`ui_theme`): a new theme, or a new
        // scale, mounts the chrome again.
        if m.as_ref().is_some_and(|l| l.dark != dark || l.k != k) {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let bottom = view.bottom.as_ref();
        let bar = bottom.map(|b| b.bar.clone());
        let speed = bottom.map(|b| b.speed.clone());
        let buttons = bottom.map(|b| (b.peeking, b.overview));
        let chip = bottom.and_then(|b| b.chip.clone());
        match m.as_mut() {
            None => {
                let live = Live {
                    dark,
                    k,
                    acts: Rc::new(RefCell::new(Vec::new())),
                    place: signal(view.place),
                    card: signal(view.card.clone()),
                    bar: signal(bar),
                    speed: signal(speed),
                    buttons: signal(buttons),
                    chip: signal(chip),
                };
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    view_of(&live),
                    dark,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(&live)) {
                        warn!(?path, error = %e, "lib driver: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                put(&live.place, view.place);
                put(&live.card, view.card.clone());
                put(&live.bar, bar);
                put(&live.speed, speed);
                put(&live.buttons, buttons);
                put(&live.chip, chip);
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

/// `v` points as the library's px.
fn px(v: f32, k: f32) -> String {
    format!("{:.1}px", v * k)
}

/// A tap that hands `a` back.
fn tap(acts: &Acts, a: DriverAct) -> impl Fn(&Event) + 'static {
    let acts = acts.clone();
    move |_: &Event| acts.borrow_mut().push(a)
}

/// The same, as a component's handler.
fn handler(acts: &Acts, a: DriverAct) -> Handler {
    Rc::new(tap(acts, a)) as Handler
}

/// A glyph of the icon face at `size` px.
fn glyph(g: &'static str, size: f32) -> View {
    el("span")
        .attr(
            "style",
            format!(
                "font-family: 'Material Symbols Rounded'; font-size: {size:.1}px; line-height: 1"
            ),
        )
        .child(g)
        .into_view()
}

/// The root: the whole window, listening nowhere, each piece placed in it
/// and listening on its own node, as egui's areas take the pointer over
/// them. Painted in egui's order: the card, the bar, the speed, the view
/// buttons, the chip.
fn view_of(l: &Live) -> View {
    let k = l.k;
    let root = format!(
        "position: absolute; left: 0; top: 0; right: 0; bottom: 0; pointer-events: none; {}",
        theme::map_vars()
    );
    el("div")
        .attr("style", root)
        .child({
            let (c, p, a) = (l.card.clone(), l.place.clone(), l.acts.clone());
            move || card_view(&c.get(), &p.get(), k, &a)
        })
        .child({
            let (s, p, a) = (l.bar.clone(), l.place.clone(), l.acts.clone());
            move || {
                let place = p.get();
                s.get().map(|v| bar_view(&v, &place, k, &a))
            }
        })
        .child({
            let (s, p) = (l.speed.clone(), l.place.clone());
            move || {
                let place = p.get();
                s.get().map(|v| speed_view(&v, &place, k))
            }
        })
        .child({
            let (s, p, a) = (l.buttons.clone(), l.place.clone(), l.acts.clone());
            move || {
                let place = p.get();
                s.get()
                    .map(|(peeking, overview)| buttons_view(peeking, overview, &place, k, &a))
            }
        })
        .child({
            let (s, p) = (l.chip.clone(), l.place.clone());
            move || {
                let place = p.get();
                s.get().map(|v| chip_view(&v, &place, k))
            }
        })
        .into_view()
}

/// A round button with an icon (`theme::round_button`): the map's plate, or —
/// `on` — the accent; a tap hands `act` back.
fn round_button(
    icon: &'static str,
    hint: &'static str,
    on: bool,
    act: DriverAct,
    k: f32,
    acts: &Acts,
) -> View {
    let (fill, rim, ink) = if on {
        ("var(--m-accent)", "var(--m-accent)", "var(--m-on-accent)")
    } else {
        ("var(--map-fill)", "var(--map-rim)", "var(--map-ink)")
    };
    el("div")
        .attr(
            "style",
            format!(
                "flex: none; width: {s}; height: {s}; box-sizing: border-box; display: flex; \
                 align-items: center; justify-content: center; border-radius: var(--m-radius-full); \
                 border: 1.5px solid {rim}; background: {fill}; color: {ink}; \
                 box-shadow: var(--m-shadow); cursor: pointer; pointer-events: auto",
                s = px(DISC, k)
            ),
        )
        .attr("title", hint)
        .attr("aria-label", hint)
        .on("click", tap(acts, act))
        .child(glyph(icon, 17.0 * k))
        .into_view()
}

// ─── The turn card ───────────────────────────────────────────────────────────

/// The turn card: the manoeuvre on its signage plate, the instruction and
/// the street, then Retry / Stop or the approach bar.
fn card_view(c: &DriverCard, p: &DriverPlace, k: f32, acts: &Acts) -> View {
    let frame = format!(
        "position: absolute; left: {m}; top: {m}; width: {w}; height: {h}; {SURFACE}; \
         padding: {pad}; overflow: hidden; pointer-events: auto",
        m = px(CARD_MARGIN, k),
        w = px(p.card_width, k),
        h = px(CARD_HEIGHT, k),
        pad = px(10.0, k),
    );
    // The column beside the plate: the content less the plate and its gap.
    let column_w = (p.card_width - 22.0 - TILE - 12.0).max(0.0);
    let plate = el("span")
        .attr(
            "style",
            format!(
                "flex: none; width: {t}; height: {t}; display: flex; align-items: center; \
                 justify-content: center; border-radius: {}; background: {}; \
                 color: var(--m-text-strong)",
                px(12.0, k),
                hex(c.plate),
                t = px(TILE, k),
            ),
        )
        .child(glyph(c.glyph, 40.0 * k));
    let title = el("span")
        .attr(
            "style",
            format!(
                "font-size: {}; font-weight: 600; color: var(--m-text-strong); line-height: 1.15; \
                 white-space: nowrap",
                px(c.title_size, k)
            ),
        )
        .child(c.title.clone());
    // The turn after this one, when it follows at once — right-aligned, a
    // footnote to the instruction.
    let then: View = match c.then {
        Some(next) => el("span")
            .attr(
                "style",
                format!(
                    "flex: none; display: flex; align-items: center; gap: {}; color: var(--m-text)",
                    px(3.0, k)
                ),
            )
            .child(
                el("span")
                    .attr(
                        "style",
                        format!("font-size: {}; color: var(--m-text-dim)", px(11.0, k)),
                    )
                    .child("then"),
            )
            .child(glyph(next, 18.0 * k))
            .into_view(),
        None => View::Empty,
    };
    let head = el("div")
        .attr(
            "style",
            format!("display: flex; align-items: center; gap: {}", px(GAP, k)),
        )
        .child(title)
        .child(el("span").attr("style", "flex: 1; min-width: 0"))
        .child(then);
    let sub: View = if c.sub.is_empty() {
        View::Empty
    } else {
        el("span")
            .attr(
                "style",
                format!(
                    "font-size: {}; color: var(--m-text); white-space: nowrap; overflow: hidden; \
                     text-overflow: ellipsis",
                    px(theme::SIZE_BODY, k)
                ),
            )
            .child(c.sub.clone())
            .into_view()
    };
    let foot: View = if c.retry || c.stop {
        let retry: View = if c.retry {
            view! {
                <Button variant=Variant::Ghost size=Size::Sm onclick={Some(handler(acts, DriverAct::Retry))} title="Ask for a route from here again">"Retry"</Button>
            }
        } else {
            View::Empty
        };
        let stop: View = if c.stop {
            let word = if c.arrived { "Done" } else { "Stop" };
            view! {
                <Button variant=Variant::Ghost size=Size::Sm onclick={Some(handler(acts, DriverAct::Done))} title="Leave navigation and put the map back">{word}</Button>
            }
        } else {
            View::Empty
        };
        el("div")
            .attr(
                "style",
                format!("display: flex; align-items: center; gap: {}", px(6.0, k)),
            )
            .child(retry)
            .child(stop)
            .into_view()
    } else if let Some(pct) = c.approach {
        approach_view(pct, column_w, k)
    } else {
        View::Empty
    };
    let column = el("div")
        .attr(
            "style",
            format!(
                "flex: 1; min-width: 0; display: flex; flex-direction: column; gap: {}",
                px(2.0, k)
            ),
        )
        .child(head)
        .child(sub)
        .child(foot);
    let row = el("div")
        .attr(
            "style",
            format!("display: flex; align-items: center; gap: {}", px(12.0, k)),
        )
        .child(plate)
        .child(column);
    // The card's own node listens (a click that does nothing), as egui's
    // area takes the pointer over it.
    el("div")
        .attr("style", frame)
        .on("click", |_: &Event| {})
        .child(row)
        .into_view()
}

/// The card's bottom edge, filling over the last stretch to the manoeuvre
/// (`driver::approach_bar`): a 4 px track in the ink at 35 %, the filled
/// part in the strong text tone, `w` wide.
fn approach_view(pct: u8, w: f32, k: f32) -> View {
    let filled = w * f32::from(pct.min(100)) / 100.0;
    let fill: View = if filled > 0.0 {
        el("span")
            .attr(
                "style",
                format!(
                    "position: absolute; left: 0; top: 0; width: {}; height: {}; \
                     border-radius: {}; background: var(--m-text-strong)",
                    px(filled, k),
                    px(4.0, k),
                    px(2.0, k),
                ),
            )
            .into_view()
    } else {
        View::Empty
    };
    // egui's `add_space(4)` above it, in a box of its own.
    el("div")
        .attr("style", format!("flex: none; padding-top: {}", px(4.0, k)))
        .child(
            el("div")
                .attr(
                    "style",
                    format!(
                        "position: relative; width: {}; height: {}; border-radius: {}; \
                         background: {}",
                        px(w, k),
                        px(4.0, k),
                        px(2.0, k),
                        alpha(theme::p().ink, 35.0),
                    ),
                )
                .child(fill),
        )
        .into_view()
}

// ─── The bottom edge ─────────────────────────────────────────────────────────

/// Where the bar's ends are: the left (and right) edge of what stands over
/// them, in from the screen's.
fn bar_end(p: &DriverPlace) -> f32 {
    ((p.w - p.bar_width) * 0.5).max(CARD_MARGIN)
}

/// The trip bar: Stop (Done), arrival, time left, distance to go, and the
/// planner; the trip's progress as a hairline along its top edge.
fn bar_view(b: &DriverBar, p: &DriverPlace, k: f32, acts: &Acts) -> View {
    let frame = format!(
        "position: absolute; left: {}; bottom: {}; width: {}; height: {}; {SURFACE}; \
         padding: {}; display: flex; align-items: center; gap: {}; pointer-events: auto",
        px(((p.w - p.bar_width) * 0.5).max(0.0), k),
        px(bar_bottom(), k),
        px(p.bar_width, k),
        px(BAR_HEIGHT, k),
        px(8.0, k),
        px(GAP, k),
    );
    let stop = if b.arrived {
        round_button(icons::CHECK, "Done", false, DriverAct::Done, k, acts)
    } else {
        round_button(
            icons::CLEAR,
            "Stop navigation",
            false,
            DriverAct::Done,
            k,
            acts,
        )
    };
    // The number big and its unit with the caption ("19" over "min left"):
    // at a phone's width three "19 min"s in 19 px ran into each other, and
    // egui's broke "18:18" over two lines.
    let stat = |value: &str, caption: &str| -> View {
        let (value, caption) = match value.split_once(' ') {
            Some((number, unit)) => (number.to_owned(), format!("{unit} {caption}")),
            None => (value.to_owned(), caption.to_owned()),
        };
        el("div")
            .attr(
                "style",
                "flex: 1 1 auto; min-width: 0; align-self: stretch; display: flex; \
                 flex-direction: column; align-items: center; justify-content: center",
            )
            .child(
                el("span")
                    .attr(
                        "style",
                        format!(
                            "font-size: {}; font-weight: 600; color: var(--m-text-strong); \
                             line-height: 1.25; white-space: nowrap",
                            px(theme::SIZE_TITLE, k)
                        ),
                    )
                    .child(value),
            )
            .child(
                el("span")
                    .attr(
                        "style",
                        format!(
                            "font-size: {}; color: var(--m-text-dim); line-height: 1.25; \
                             white-space: nowrap",
                            px(11.0, k)
                        ),
                    )
                    .child(caption),
            )
            .into_view()
    };
    let menu = round_button(
        icons::MENU,
        "Route — destination, turn list, travel mode",
        false,
        DriverAct::OpenPlanner,
        k,
        acts,
    );
    // How much of the trip is behind you, along the bar's top edge, inset
    // from its ends — drawn over the frame, as egui's line is, so it costs
    // the bar no height. Placed in the frame's padding box, inside its rim.
    let line_w = (p.bar_width - 2.0 * LINE_INSET).max(0.0);
    let track = |w: f32, bg: String| -> View {
        el("span")
            .attr(
                "style",
                format!(
                    "position: absolute; left: {}; top: 0; width: {}; height: {}; background: {bg}",
                    px(LINE_INSET - 1.0, k),
                    px(w, k),
                    px(LINE_W, k),
                ),
            )
            .into_view()
    };
    let driven = line_w * f32::from(b.driven.min(1000)) / 1000.0;
    let progress: View = if driven > 0.0 {
        track(driven, hex(ROUTE_BLUE))
    } else {
        View::Empty
    };
    el("div")
        .attr("style", frame)
        .on("click", |_: &Event| {})
        .child(stop)
        .child(stat(
            &b.arrival,
            if b.arrived { "arrived" } else { "arrival" },
        ))
        .child(stat(&b.time_left, "left"))
        .child(stat(&b.to_go, "to go"))
        .child(menu)
        .child(track(line_w, "var(--m-border)".to_owned()))
        .child(progress)
        .into_view()
}

/// The speed above the bar's left end, the limit sign over it. Over the
/// limit turns the *speed* red, not the sign.
fn speed_view(s: &DriverSpeed, p: &DriverPlace, k: f32) -> View {
    let column = format!(
        "position: absolute; left: {}; bottom: {}; width: {}; display: flex; \
         flex-direction: column; align-items: center; gap: {}; pointer-events: auto",
        px(bar_end(p), k),
        px(above_bar(), k),
        px(SPEED_DISC, k),
        px(6.0, k),
    );
    let sign: View = match s.limit {
        Some(limit) => el("span")
            .attr(
                "style",
                format!(
                    "flex: none; width: {d}; height: {d}; box-sizing: border-box; display: flex; \
                     align-items: center; justify-content: center; \
                     border-radius: var(--m-radius-full); border: {} solid {}; background: {}; \
                     color: {}; font-size: {}; line-height: 1",
                    px(SIGN_RING, k),
                    hex(SIGN_RED),
                    hex(SIGN_WHITE),
                    hex(egui::Color32::BLACK),
                    px(if limit >= 100 { 10.0 } else { 12.0 }, k),
                    d = px(SIGN_R * 2.0, k),
                ),
            )
            .child(limit.to_string())
            .into_view(),
        None => View::Empty,
    };
    let ink = if s.over {
        hex(SIGN_RED)
    } else {
        "var(--m-text-strong)".to_owned()
    };
    // egui centres the number 5 above the disc's middle and "km/h" 13 below
    // it: a 22 + 2 + 10 stack centred a point low.
    let disc = el("div")
        .attr(
            "style",
            format!(
                "flex: none; width: {d}; height: {d}; box-sizing: border-box; display: flex; \
                 flex-direction: column; align-items: center; justify-content: center; \
                 gap: {}; padding-top: {}; border-radius: var(--m-radius-full); \
                 background: var(--m-surface); border: 1px solid var(--m-border)",
                px(2.0, k),
                px(2.0, k),
                d = px(SPEED_DISC, k),
            ),
        )
        .child(
            el("span")
                .attr(
                    "style",
                    format!(
                        "font-size: {}; font-weight: 600; line-height: 1; color: {ink}",
                        px(22.0, k)
                    ),
                )
                .child(s.speed.clone()),
        )
        .child(
            el("span")
                .attr(
                    "style",
                    format!(
                        "font-size: {}; line-height: 1; color: var(--m-text-dim)",
                        px(theme::SIZE_TINY, k)
                    ),
                )
                .child("km/h"),
        );
    el("div")
        .attr("style", column)
        .on("click", |_: &Event| {})
        .child(sign)
        .child(disc)
        .into_view()
}

/// Re-centre (while the view is held off the rig) and Overview, above the
/// bar's right end.
fn buttons_view(peeking: bool, overview: bool, p: &DriverPlace, k: f32, acts: &Acts) -> View {
    let row = format!(
        "position: absolute; right: {}; bottom: {}; display: flex; align-items: center; \
         gap: {}; pointer-events: auto",
        px(bar_end(p), k),
        px(above_bar(), k),
        px(GAP, k),
    );
    let recentre: View = if peeking {
        round_button(
            icons::HERE,
            "Put the camera back behind the arrow",
            false,
            DriverAct::Recentre,
            k,
            acts,
        )
    } else {
        View::Empty
    };
    let hint = if overview {
        "Back to following the road"
    } else {
        "See the rest of the route"
    };
    el("div")
        .attr("style", row)
        .child(recentre)
        .child(round_button(
            icons::CAT_MAP,
            hint,
            overview,
            DriverAct::ToggleOverview,
            k,
            acts,
        ))
        .into_view()
}

/// `theme::LABEL_CHIP` as CSS: a translucent colour goes through `alpha`.
fn chip_fill() -> String {
    let [r, g, b, a] = theme::LABEL_CHIP.to_srgba_unmultiplied();
    alpha(egui::Color32::from_rgb(r, g, b), f32::from(a) / 2.55)
}

/// What the position is worth, centred above the bar in `theme::chip_frame`.
fn chip_view(text: &str, p: &DriverPlace, k: f32) -> View {
    let band = format!(
        "position: absolute; left: 0; bottom: {}; width: {}; display: flex; \
         justify-content: center; pointer-events: none",
        px(above_bar(), k),
        px(p.w, k),
    );
    let pad = theme::CHIP_PADDING;
    let chip = format!(
        "box-sizing: border-box; width: {}; padding: {} {}; background: {}; border-radius: {}; \
         color: {}; font-size: {}; white-space: nowrap; overflow: hidden; \
         text-overflow: ellipsis; pointer-events: auto",
        px(p.chip_width + 2.0 * pad.x, k),
        px(pad.y, k),
        px(pad.x, k),
        chip_fill(),
        px(f32::from(theme::RADIUS_SM), k),
        hex(theme::DARK.warn),
        px(theme::SIZE_CHIP, k),
    );
    el("div")
        .attr("style", band)
        .child(
            el("div")
                .attr("style", chip)
                .on("click", |_: &Event| {})
                .child(text.to_owned()),
        )
        .into_view()
}
