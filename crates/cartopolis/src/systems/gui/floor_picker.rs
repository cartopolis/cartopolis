//! The floor picker: the storeys of the building being looked into, by the names
//! its design gives them (`systems::cutaway`).
//!
//! A column at the right edge, top storey first, as a lift's buttons and every
//! indoor map's level switcher are — so it reads as "floors" before a word of it
//! is read. It appears only for the building card's Look inside — the card is
//! opened by double-tapping a building, so asking for the floors is always a
//! choice. Walking in still cuts the building away; it no longer opens a panel
//! over the map uninvited. Tapping a storey shows that storey; the
//! storey the avatar is on carries a dot, and once another has been picked a
//! "Follow me" row hands the choice back to the avatar's feet.
//!
//! **One vocabulary with the editor.** The storey names, the room line ("In the
//! Keuken") and the Floor plan button are the same words and the same building
//! the atlas floor-plan editor shows, because the picker is the read side of what
//! the owner wrote there: what is drawn in `/building/{pand}` is what this lists.
//!
//! Drawn by the library (`lib_chrome`) at the right edge, and hidden
//! by whatever owns the screen it would float over — driver mode, the main menu,
//! the on-screen keyboard and, on a phone, an open panel.

use bevy::prelude::*;

use crate::resources::RightPanel;
use crate::systems::building_facts::BuildingFacts;
use crate::systems::cutaway::Cutaway;
use crate::systems::data_layers::{open_url, open_url_supported};
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;

/// Width of the column, points. Wide enough for "Ground floor" and a dot.
const WIDTH: f32 = 150.0;

/// The picker as the library draws it (`lib_chrome`, through
/// `dock::ChromeLib`): [`drive_floor_picker`] decides whether it shows and
/// what it lists, and does what a tap asks.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct FloorsView {
    /// Points from the screen's right edge and top.
    pub right: f32,
    pub top: f32,
    /// The column's width inside its frame.
    pub width: f32,
    /// "In the Keuken".
    pub room: Option<String>,
    /// Top storey first: its index, its name, whether it is shown, whether
    /// the avatar is on it.
    pub storeys: Vec<(usize, String, bool, bool)>,
    /// Another storey than the avatar's was picked: "Follow me" hands the
    /// choice back.
    pub follow: bool,
    pub designed: bool,
    /// The floor-plan editor can be opened from here.
    pub plan: bool,
}

/// Decide the picker while the building card is looking inside a building,
/// publish it for the library's edition (`lib_chrome`) and do its taps.
#[allow(clippy::too_many_arguments)]
pub fn drive_floor_picker(
    metrics: Res<UiMetrics>,
    mut cutaway: ResMut<Cutaway>,
    facts: Res<BuildingFacts>,
    right_panel: Res<RightPanel>,
    menu: Res<crate::systems::ui::MainMenu>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    mut chrome: ResMut<crate::systems::dock::MapChrome>,
) {
    use crate::systems::dock::{set_if_changed, ChromeAct};
    let acts = chrome.lib.take_acts(ChromeAct::is_floor);
    // Only the card's Look inside asks for the floors, once the interior has
    // been built, and not under what owns the screen.
    let shown = cutaway.looking.is_some()
        && !cutaway.storeys.is_empty()
        && !(driver.active || menu.open || keyboard.is_open())
        && !(metrics.compact() && !right_panel.is(RightPanel::None));
    let Some(id) = cutaway.building.clone().filter(|_| shown) else {
        set_if_changed(&mut chrome.lib.floors, None);
        return;
    };

    let view = FloorsView {
        // Under the map's column of round buttons on the same edge
        // (`dock::right_column`); in the middle of the edge it ran into them.
        right: theme::OSD_MARGIN,
        top: chrome.right_bottom + theme::OSD_GROUP_GAP,
        width: WIDTH,
        room: cutaway.room.clone(),
        // Top storey first: a lift, and every indoor map, counts upward.
        storeys: (0..cutaway.storeys.len())
            .rev()
            .map(|k| {
                (
                    k,
                    cutaway.storeys[k].clone(),
                    k == cutaway.storey,
                    cutaway.avatar_storey == Some(k),
                )
            })
            .collect(),
        follow: cutaway.picked.is_some() && cutaway.avatar_storey.is_some(),
        designed: cutaway.designed,
        plan: open_url_supported(),
    };
    set_if_changed(&mut chrome.lib.floors, Some(view));

    let mut pick: Option<Option<usize>> = None;
    let mut close = false;
    let mut plan = false;
    for act in acts {
        match act {
            ChromeAct::FloorPick(k) => pick = Some(k),
            ChromeAct::FloorClose => close = true,
            ChromeAct::FloorPlan => plan = true,
            _ => {}
        }
    }
    let open = plan
        .then(|| {
            facts
                .building(&id)
                .map(|f| crate::systems::building_card::floor_plan_url(&id, &f.footprint))
        })
        .flatten();

    if let Some(k) = pick {
        cutaway.picked = k;
    }
    if close {
        cutaway.stop_looking();
    }
    if let Some(url) = open {
        open_url(&url);
    }
}
