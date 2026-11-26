//! The place names over the map drawn by the component library
//! (`lib_host`), beside egui's (`place_names::render_place_names`): towns
//! and countries as banners, districts in spaced capitals, stations as a
//! ringed disc.
//!
//! On with `CARTO_UI_LIB=place_names` (or `1`), on the web
//! `?ui=place_names`. egui's system still chooses the names (by distance,
//! then by room), places and measures them and opens the ring a name opens;
//! it publishes the frame's names in `PlaceNames::lib_view` and paints none.
//! A tap here goes back as `PlaceNames::lib_tapped` and opens the same ring
//! on the same place.
//!
//! Drawn through `lib_pins::Anchored`, the one keyed list everything
//! anchored to the world goes through: a name that only moved restyles its
//! node, one that is left out for room is hidden, not unmounted.
//!
//! What differs from egui's: the banners and the halo take the theme's
//! surface and text (`--m-surface`, `--m-text-strong`) where egui's are the
//! dark palette in both themes; the halo is four copies of the name, egui's
//! eight; a station's train is the icon font's, not three painted strokes;
//! the library has no hover, so a banner is not ringed under the pointer.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::Event;
use mithril::prelude::*;

use crate::systems::lib_pins::{boxed, px_scale, Anchored, Spot};
use crate::systems::lib_settings::LibScreens;
use crate::systems::place_names::{NameLook, PlaceNames};
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The names' key in the library's host.
const KEY: &str = "place_names";

/// How far the halo's copies stand off the name, px.
const HALO: f32 = 1.2;

#[derive(Clone)]
struct Live {
    names: Anchored<NameLook>,
    /// The key of the name tapped since the last frame.
    tapped: Rc<RefCell<Option<String>>>,
    /// The theme it was mounted in: another remounts it.
    dark: bool,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts the names once, mirrors egui's frame of names into them, and
/// hands a tap back. `PostUpdate`, before `DriveUi`.
pub fn drive_lib_place_names(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut names: ResMut<PlaceNames>,
    metrics: Res<UiMetrics>,
) {
    if !lib.place_names {
        return;
    }
    if !names.lib {
        names.lib = true;
    }
    let k = px_scale(metrics.ui_scale());
    let dark = theme::p().ink == theme::DARK.ink;
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        if m.as_ref().is_some_and(|live| live.dark != dark) {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let live = m.get_or_insert_with(|| {
            let live = Live {
                names: Anchored::new(),
                tapped: Rc::new(RefCell::new(None)),
                dark,
            };
            crate::systems::lib_host::mount_keyed(KEY, view_of(&live), dark, theme::touch_layout());
            live
        });
        if let Some(key) = live.tapped.borrow_mut().take() {
            names.lib_tapped = Some(key);
        }
        live.names.sync(names.lib_view.iter().map(|n| {
            (
                n.key.clone(),
                Spot::at(n.at.x * k, n.at.y * k),
                n.look.clone(),
            )
        }));
    });
}

fn view_of(l: &Live) -> View {
    let tapped = l.tapped.clone();
    // Under the pins (-1), as egui orders them: a saved place stays on top
    // of a town's name and wins the tap.
    l.names.view(-2, move |key, look| {
        let tap: Handler = {
            let (tapped, key) = (tapped.clone(), key.to_string());
            Rc::new(move |_: &Event| *tapped.borrow_mut() = Some(key.clone()))
        };
        name(look, tap)
    })
}

/// One name around its point, which is its item's origin.
fn name(look: &NameLook, tap: Handler) -> View {
    match look {
        NameLook::Banner {
            name,
            size,
            population,
            pill,
            badge,
            dot,
        } => banner(name, *size, population.as_deref(), *pill, *badge, *dot, tap),
        NameLook::Station {
            name,
            width,
            height,
        } => station(name.as_deref(), *width, *height, tap),
        NameLook::District {
            text,
            size,
            width,
            height,
        } => district(text, *size, *width, *height, tap),
    }
}

/// A town's or a country's banner (`place_names::banner`): the pill with the
/// population's badge and the name, over a dot where a town is.
fn banner(
    name: &str,
    size: f32,
    population: Option<&str>,
    pill: [f32; 4],
    badge: f32,
    dot: bool,
    tap: Handler,
) -> View {
    let [left, top, w, h] = pill;
    let lead = if population.is_some() { 3.0 } else { 9.0 };
    let frame = boxed(
        left,
        top,
        w,
        h,
        &format!(
            "box-sizing: border-box; display: flex; align-items: center; padding-left: {lead}px; border-radius: var(--m-radius-full); background: var(--m-surface); border: 1px solid var(--m-border); white-space: nowrap"
        ),
    );
    let badge: View = match population {
        Some(pop) => {
            let style = format!(
                "flex: none; display: flex; align-items: center; justify-content: center; width: {badge:.1}px; height: {:.1}px; margin-right: 6px; border-radius: var(--m-radius-full); background: var(--m-accent); color: var(--m-on-accent); font-size: {:.1}px; font-weight: 600; line-height: 1",
                (h - 6.0).max(0.0),
                size - 2.5
            );
            let pop = pop.to_string();
            view! { <span style={style}>{pop}</span> }
        }
        None => View::Empty,
    };
    let words = format!(
        "color: var(--m-text-strong); font-size: {size:.1}px; font-weight: 600; line-height: 1; white-space: nowrap"
    );
    let dot: View = if dot {
        let style = boxed(
            -4.0,
            -4.0,
            8.0,
            8.0,
            "box-sizing: border-box; border-radius: 4px; background: var(--m-surface); border: 1.5px solid var(--m-text-strong)",
        );
        let tap = tap.clone();
        view! { <div style={style} on:click={move |e: &Event| tap(e)}></div> }
    } else {
        View::Empty
    };
    let (name, title) = (name.to_string(), name.to_string());
    view! {
        {dot}
        <div style={frame} title={title} on:click={move |e: &Event| tap(e)}>
            {badge}
            <span style={words}>{name}</span>
        </div>
    }
}

/// A station (`place_names::station`): a disc ringed in the accent with a
/// train in it, and the name beside it, haloed, when it is a town's own.
fn station(name: Option<&str>, width: f32, height: f32, tap: Handler) -> View {
    const R: f32 = 9.0;
    let disc = boxed(
        -R,
        -R,
        2.0 * R,
        2.0 * R,
        "box-sizing: border-box; display: flex; align-items: center; justify-content: center; border-radius: 9px; background: var(--m-surface); border: 1.6px solid var(--m-accent); color: var(--m-text-strong); font-family: 'Material Symbols Rounded'; font-size: 12px; line-height: 1",
    );
    let label: View = match name {
        Some(name) => haloed(
            name,
            R + 4.0,
            -height / 2.0,
            width,
            height,
            "font-size: 11.5px; font-weight: 600",
        ),
        None => View::Empty,
    };
    let title = name.unwrap_or("Station").to_string();
    view! {
        <div style={disc} title={title} on:click={move |e: &Event| tap(e)}>{icons::TRANSPORT}</div>
        {label}
    }
}

/// A district (`place_names::district`): its name in spaced capitals,
/// haloed, centred on its point, with a little room round it for a finger.
fn district(text: &str, size: f32, width: f32, height: f32, tap: Handler) -> View {
    let (pad_x, pad_y) = (4.0, 5.0);
    let frame = boxed(
        -width / 2.0 - pad_x,
        -height / 2.0 - pad_y,
        width + 2.0 * pad_x,
        height + 2.0 * pad_y,
        "",
    );
    let font = format!("font-size: {size:.1}px; font-weight: 600; letter-spacing: 1.4px");
    let words = haloed(text, pad_x, pad_y, width, height, &font);
    let title = text.to_string();
    view! {
        <div style={frame} title={title} on:click={move |e: &Event| tap(e)}>{words}</div>
    }
}

/// Text with a halo in the surface's colour, so it reads over roofs, water
/// and fields alike: four copies a little off it, then the text.
fn haloed(text: &str, left: f32, top: f32, width: f32, height: f32, font: &str) -> View {
    let copy = |dx: f32, dy: f32, ink: &str| -> View {
        let style = boxed(
            dx,
            dy,
            width + 2.0,
            height,
            &format!("color: {ink}; white-space: nowrap; line-height: 1; {font}"),
        );
        let text = text.to_string();
        view! { <span style={style}>{text}</span> }
    };
    let halo = "var(--m-surface)";
    let frame = boxed(left, top, width + 2.0, height, "");
    view! {
        <div style={frame}>
            {copy(-HALO, 0.0, halo)}
            {copy(HALO, 0.0, halo)}
            {copy(0.0, -HALO, halo)}
            {copy(0.0, HALO, halo)}
            {copy(0.0, 0.0, "var(--m-text-strong)")}
        </div>
    }
}
