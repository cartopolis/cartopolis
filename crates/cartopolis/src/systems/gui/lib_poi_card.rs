//! The card on a tapped point of interest drawn by the component library
//! (`lib_host`): its name, what it is and where, and the way to its website
//! and to its OSM node.
//!
//! `data_layers::drive_poi_card` projects the POI onto the screen each frame
//! and publishes the card in `PoiCard::view` (`None` when there is none, or
//! while the POI is behind the camera). A tap here is done by
//! `data_layers::poi_act`.
//!
//! **Placement.** Its bottom centre on the point above the POI's pole. The
//! card hangs off a box of no size on that point (as `lib_pins`' markers
//! do), so it needs no measuring and nothing but the card can hold the
//! pointer — a box under the other screens would block their taps in
//! `bevy_ui`'s picking. It takes the theme's card surface, as the library's
//! place names do, and is always 260 px wide with its padding (centring it
//! needs its width).

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::data_layers::{
    open_url_supported, poi_act, DataLayers, PoiAction, PoiCard, PoiCardView, NO_BROWSER,
};
use crate::systems::lib_place_card::icon;
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The card's name in the library's host.
const KEY: &str = "poi_card";

/// The card's width, px, its padding included: 240 of content and 10 a
/// side.
const WIDTH: f32 = 260.0;

/// Where the card stands, px in the library's root: the point its bottom
/// centre sits on, and its width.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
struct Placement {
    x: f32,
    y: f32,
    width: f32,
}

impl Placement {
    /// For the point `at`, UI points. The library's px are UI points on the
    /// Bevy backend; as DOM they are the page's, `ui_scale` times larger.
    fn of(at: (f32, f32), ui_scale: f32) -> Self {
        let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            ui_scale
        } else {
            1.0
        };
        Placement {
            x: at.0 * k,
            y: at.1 * k,
            width: WIDTH * k,
        }
    }

    /// A box of no size on the point: it holds no pointer, under either
    /// backend, and the card hangs off it.
    fn style(&self) -> String {
        format!(
            "position: absolute; left: {:.0}px; top: {:.0}px; width: 0px; height: 0px",
            self.x, self.y
        )
    }

    /// The card, its bottom on the point and centred over it.
    fn card(&self) -> String {
        format!(
            "position: absolute; left: {:.0}px; bottom: 0px; width: {:.0}px; \
             display: flex; flex-direction: column; gap: 4px; padding: 10px 10px 12px 14px; \
             background: var(--m-surface); border: 1px solid var(--m-border); \
             border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
            -self.width / 2.0,
            self.width
        )
    }
}

/// What the card says: another POI mounts it again.
#[derive(Clone, PartialEq)]
struct Content {
    title: String,
    subtitle: String,
    website: Option<String>,
    osm_id: i64,
}

impl Content {
    fn of(v: &PoiCardView) -> Self {
        Content {
            title: v.title.clone(),
            subtitle: v.subtitle.clone(),
            website: v.website.clone(),
            osm_id: v.osm_id,
        }
    }
}

#[derive(Clone)]
struct Live {
    content: Content,
    placement: Signal<Placement>,
    acts: Rc<RefCell<Vec<PoiAction>>>,
}

impl Live {
    fn act(&self, a: PoiAction) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the POI card. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_poi_card(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    card: Res<PoiCard>,
    mut dl: ResMut<DataLayers>,
    metrics: Res<UiMetrics>,
) {
    if !lib.poi_card {
        return;
    }
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<PoiAction> = std::mem::take(&mut *live.acts.borrow_mut());
            for a in acts {
                poi_act(&mut dl, a);
            }
        }
        // A tap may have closed it: `data_layers::drive_poi_card` publishes
        // again next frame.
        let view = card.view.clone().filter(|_| dl.popup.is_some());
        let stale = m
            .as_ref()
            .is_some_and(|live| view.as_ref().is_none_or(|v| Content::of(v) != live.content));
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let Some(view) = view else {
            return;
        };
        let placement = Placement::of(view.at, metrics.ui_scale());
        match m.as_mut() {
            None => {
                let live = Live {
                    content: Content::of(&view),
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
                        warn!(?path, error = %e, "lib POI card: could not write the HTML");
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

/// The card: the name and ✕, what it is and where, Website and OSM.
fn view_of(l: &Live) -> View {
    let c = &l.content;
    let frame = {
        let placement = l.placement.clone();
        move || placement.get().style()
    };
    let inner = {
        let placement = l.placement.clone();
        move || placement.get().card()
    };
    let title = c.title.clone();
    let subtitle = c.subtitle.clone();
    // Both buttons hand their URL to `open_url`, which cannot reach a
    // browser on every platform: greyed out there, and the hover says why.
    let can_open = open_url_supported();
    let website = match (&c.website, can_open) {
        (None, _) => View::Empty,
        (Some(_), true) => view! {
            <Button size=Size::Sm icon={icon(icons::OPEN_EXTERNAL)} onclick={l.act(PoiAction::Website)}>"Website"</Button>
        },
        (Some(_), false) => view! {
            <Button size=Size::Sm icon={icon(icons::OPEN_EXTERNAL)} disabled=true title={NO_BROWSER}>"Website"</Button>
        },
    };
    let osm = if can_open {
        view! {
            <Button size=Size::Sm variant=Variant::Ghost onclick={l.act(PoiAction::Osm)}>"OSM"</Button>
        }
    } else {
        view! {
            <Button size=Size::Sm variant=Variant::Ghost disabled=true title={NO_BROWSER}>"OSM"</Button>
        }
    };
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too.
    view! {
        <div style={frame}>
            <div style={inner} on:click={|_: &Event| {}}>
                <div style="display: flex; align-items: center; gap: 8px">
                    <span style="flex: 1; min-width: 0; font-size: 14px; font-weight: 600; color: var(--m-text-strong)">{title}</span>
                    <CloseButton onclick={l.act(PoiAction::Close)} />
                </div>
                <span style="font-size: 12.5px; color: var(--m-text-dim)">{subtitle}</span>
                <div style="display: flex; flex-wrap: wrap; gap: 8px; margin-top: 8px">
                    {website}{osm}
                </div>
            </div>
        </div>
    }
}
