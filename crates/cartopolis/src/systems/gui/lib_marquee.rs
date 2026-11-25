//! The builder's rubber band drawn by the component library (`lib_host`),
//! beside egui's (`picking::render_marquee`): the rectangle a drag over
//! empty ground spreads while it selects every object inside it.
//!
//! On with `CARTO_UI_LIB=marquee` (or `1`), on the web `?ui=lib`. The drag
//! stays where it is (`picking::prim_picking` measures it, Bevy's input);
//! egui's system steps aside (`MarqueeLib::lib`) and publishes the
//! rectangle in `MarqueeLib::view`, which this draws: the accent's rim over
//! a faint wash of it, as egui's (`accent` at 28 / 255, `accent_bright` 1 px
//! outside the rectangle). It listens nowhere: the drag goes on under it.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;

use crate::systems::egui_manager::EguiRef;
use crate::systems::lib_settings::LibScreens;
use crate::systems::picking::MarqueeLib;
use crate::systems::theme;
use crate::systems::theme::tokens::{alpha, hex};

/// The rubber band's name in the library's host.
const KEY: &str = "marquee";

/// egui's wash: the accent at 28 of 255.
const WASH_PCT: f32 = 28.0 / 255.0 * 100.0;

/// The rectangle, in the library's px.
#[derive(Clone, Copy, PartialEq)]
struct Band {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

impl Band {
    /// `[left, top, right, bottom]` in egui points, at `k` px per point,
    /// grown by the 1 px rim egui strokes outside it.
    fn of(r: [f32; 4], k: f32) -> Self {
        Band {
            left: r[0] * k - 1.0,
            top: r[1] * k - 1.0,
            width: (r[2] - r[0]).max(0.0) * k + 2.0,
            height: (r[3] - r[1]).max(0.0) * k + 2.0,
        }
    }
}

#[derive(Clone)]
struct Live {
    band: Signal<Band>,
    /// The rim and the wash, from the palette the band was mounted in.
    rim: String,
    wash: String,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, moves and unmounts the rubber band. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_marquee(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    bridge: Option<ResMut<MarqueeLib>>,
    egui: EguiRef,
) {
    if !lib.marquee {
        return;
    }
    let Some(mut bridge) = bridge else {
        return;
    };
    if !bridge.lib {
        bridge.lib = true;
    }
    // The library's px are egui's points on the Bevy backend; as DOM they
    // are the page's (`lib_place_card::Placement::over_the_dock`).
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        egui.ui_scale()
    } else {
        1.0
    };
    let want = bridge.view.map(|r| Band::of(r, k));
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        match (want, m.as_ref()) {
            (None, None) => {}
            (None, Some(_)) => {
                *m = None;
                crate::systems::lib_host::clear_keyed(KEY);
            }
            (Some(band), None) => {
                let p = theme::p();
                let live = Live {
                    band: signal(band),
                    rim: hex(p.accent_bright),
                    wash: alpha(p.accent, WASH_PCT),
                };
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    view_of(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(&live)) {
                        warn!(?path, error = %e, "lib marquee: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            (Some(band), Some(live)) => {
                if live.band.get_untracked() != band {
                    live.band.set(band);
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

/// The whole window, listening nowhere, and the band in it.
fn view_of(l: &Live) -> View {
    let band = {
        let (band, rim, wash) = (l.band.clone(), l.rim.clone(), l.wash.clone());
        move || {
            let b = band.get();
            format!(
                "position: absolute; left: {:.0}px; top: {:.0}px; width: {:.0}px; height: {:.0}px; \
                 box-sizing: border-box; border: 1px solid {rim}; background: {wash}; \
                 pointer-events: none",
                b.left, b.top, b.width, b.height
            )
        }
    };
    view! {
        <div style="position: absolute; left: 0; top: 0; right: 0; bottom: 0; pointer-events: none">
            <div style={band}></div>
        </div>
    }
}
