//! The community filter chip drawn by the component library (`lib_host`):
//! while only one community's posts are on the map, whose they are and how
//! many, the community's page a tap on its name, and ✕ to show every
//! community's again.
//!
//! `community_posts::drive_filter_chip` decides whether the chip shows —
//! the driver bar, the camera, the World view and an open panel each hide
//! it, and a filter that cannot hold is forgotten — and publishes what it
//! says in `Posts::chip_view`. Its two taps are done here, through
//! `community_posts::chip_act`.
//!
//! **Placement.** Under the search bar, centred between the two
//! columns of round buttons that come down the edges (`dock`), the name cut
//! to fit there. The band it is centred in does not listen, so a tap beside
//! the chip reaches the map (on the web, `pointer-events: none` on it).

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
use uuid::Uuid;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::RightPanel;
use crate::systems::communities::Communities;
use crate::systems::community_posts::{chip_act, ChipAction};
use crate::systems::lib_place_card::icon;
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The chip's name in the library's host.
const KEY: &str = "filter_chip";

/// The band the chip is centred in, px in the library's root.
#[derive(Clone, Copy, PartialEq, Default)]
struct Placement {
    left: f32,
    top: f32,
    width: f32,
}

impl Placement {
    fn of(w: f32, ui_scale: f32) -> Self {
        let edge = theme::OSD_MARGIN + theme::ROUND_BUTTON_SIZE + theme::OSD_GAP;
        let width = (w - 2.0 * edge).max(0.0);
        let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            ui_scale
        } else {
            1.0
        };
        Placement {
            left: ((w - width) / 2.0).max(0.0) * k,
            top: crate::systems::dock::reserved_top() * k,
            width: width * k,
        }
    }

    fn style(&self) -> String {
        format!(
            "position: absolute; left: {:.0}px; top: {:.0}px; width: {:.0}px; \
             display: flex; justify-content: center; pointer-events: none",
            self.left, self.top, self.width
        )
    }
}

#[derive(Clone)]
struct Live {
    /// The community the chip was mounted for: another mounts it again.
    community: Uuid,
    words: Signal<String>,
    placement: Signal<Placement>,
    acts: Rc<RefCell<Vec<ChipAction>>>,
}

impl Live {
    fn act(&self, a: ChipAction) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the chip. `PostUpdate`, before `DriveUi`,
/// as every library screen.
pub fn drive_lib_filter_chip(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut cs: ResMut<Communities>,
    mut right_panel: ResMut<RightPanel>,
    metrics: Res<UiMetrics>,
) {
    if !lib.filter_chip {
        return;
    }
    let placement = Placement::of(metrics.viewport_pts().x, metrics.ui_scale());
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame.
        if let Some(live) = m.as_ref() {
            let acts: Vec<ChipAction> = std::mem::take(&mut *live.acts.borrow_mut());
            for a in acts {
                chip_act(&mut cs, &mut right_panel, a);
            }
        }
        // Cleared by a tap just now, or hidden by `drive_filter_chip`.
        let shown = cs
            .posts
            .chip_view
            .clone()
            .filter(|v| cs.posts.only == Some(v.community));
        let stale = m
            .as_ref()
            .is_some_and(|live| shown.as_ref().map(|v| v.community) != Some(live.community));
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let Some(shown) = shown else {
            return;
        };
        let words = shown.words();
        match m.as_mut() {
            None => {
                let live = Live {
                    community: shown.community,
                    words: signal(words),
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
                        warn!(?path, error = %e, "lib filter chip: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                if live.words.get_untracked() != words {
                    live.words.set(words);
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

/// The chip: the public dot, whose posts and how many (a tap opens the
/// community's page), and ✕.
fn view_of(l: &Live) -> View {
    let band = {
        let placement = l.placement.clone();
        move || placement.get().style()
    };
    let words = {
        let words = l.words.clone();
        move || words.get()
    };
    let open = {
        let acts = l.acts.clone();
        move |_: &Event| acts.borrow_mut().push(ChipAction::Open)
    };
    // The chip's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too.
    view! {
        <div style={band}>
            <div on:click={|_: &Event| {}} style="pointer-events: auto; max-width: 100%; display: flex; align-items: center; gap: 8px; padding: 4px 4px 4px 16px; background: var(--m-surface); border: 1px solid var(--m-border); border-radius: var(--m-radius-full); box-shadow: var(--m-shadow-lg)">
                <span style="flex: none; width: 10px; height: 10px; border-radius: var(--m-radius-full); background: var(--m-info)"></span>
                <span title="Open the community's page" on:click={open} style="flex: 1 1 auto; min-width: 0; overflow: hidden; white-space: nowrap; color: var(--m-text-strong); font-size: var(--m-font-size-sm); cursor: pointer">{words}</span>
                <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::CLEAR)} icon_only=true onclick={l.act(ChipAction::Clear)} title="Show every community's posts" />
            </div>
        </div>
    }
}
