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
//! A floating `Area`, outside the panel chain like the building card, and hidden
//! by whatever owns the screen it would float over — driver mode, the main menu,
//! the on-screen keyboard and, on a phone, an open panel.

use crate::systems::egui_manager::EguiRef;
use bevy::prelude::*;

use crate::resources::RightPanel;
use crate::systems::building_facts::BuildingFacts;
use crate::systems::cutaway::Cutaway;
use crate::systems::data_layers::{open_url, open_url_supported};
use crate::systems::icons;
use crate::systems::theme;

/// Width of the column, points. Wide enough for "Ground floor" and a dot.
const WIDTH: f32 = 150.0;

/// The picker as the library's edition draws it (`lib_chrome`, through
/// `dock::ChromeLib`): egui's system decides whether it shows and what it
/// lists, and does what a tap asks.
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

/// Draw the picker while the building card is looking inside a building.
#[allow(clippy::too_many_arguments)]
pub fn render_floor_picker(
    egui: EguiRef,
    mut cutaway: ResMut<Cutaway>,
    facts: Res<BuildingFacts>,
    right_panel: Res<RightPanel>,
    menu: Res<crate::systems::ui::MainMenu>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    mut chrome: ResMut<crate::systems::dock::MapChrome>,
) {
    use crate::systems::dock::{set_if_changed, ChromeAct};
    // The library's edition (`lib_chrome`): its taps, and whether egui draws.
    let lib = chrome.lib.on;
    let acts = chrome.lib.take_acts(ChromeAct::is_floor);
    // Only the card's Look inside asks for the floors, once the interior has
    // been built, and not under what owns the screen.
    let shown = cutaway.looking.is_some()
        && !cutaway.storeys.is_empty()
        && !(driver.active || menu.open || keyboard.is_open())
        && !(egui.compact() && !right_panel.is(RightPanel::None));
    let Some(id) = cutaway.building.clone().filter(|_| shown) else {
        set_if_changed(&mut chrome.lib.floors, None);
        return;
    };

    let mut pick: Option<Option<usize>> = None;
    let mut close = false;
    let mut open: Option<String> = None;
    let mut plan = false;
    if lib {
        let view = FloorsView {
            right: theme::OSD_MARGIN,
            top: chrome.right_bottom + theme::OSD_GROUP_GAP,
            width: WIDTH,
            room: cutaway.room.clone(),
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
        for act in acts {
            match act {
                ChromeAct::FloorPick(k) => pick = Some(k),
                ChromeAct::FloorClose => close = true,
                ChromeAct::FloorPlan => plan = true,
                _ => {}
            }
        }
    } else {
        set_if_changed(&mut chrome.lib.floors, None);
    }
    let ctx = egui.ctx();
    if !lib {
        egui::Area::new(egui::Id::new("floor_picker"))
            // Under the map's column of round buttons on the same edge
            // (`dock::right_column`); in the middle of the edge it ran into them.
            .anchor(
                egui::Align2::RIGHT_TOP,
                egui::vec2(
                    -theme::OSD_MARGIN,
                    chrome.right_bottom + theme::OSD_GROUP_GAP,
                ),
            )
            .show(ctx, |ui| {
                theme::overlay_frame().show(ui, |ui| {
                    ui.set_width(WIDTH);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Floors").strong().size(theme::SIZE_SUB));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = theme::close_button(ui).clicked();
                        });
                    });
                    if let Some(room) = &cutaway.room {
                        ui.label(
                            egui::RichText::new(format!("In the {room}"))
                                .size(theme::SIZE_SUB)
                                .color(theme::p().accent),
                        );
                    }
                    ui.add_space(4.0);
                    // Top storey first: a lift, and every indoor map, counts upward.
                    for k in (0..cutaway.storeys.len()).rev() {
                        let mut label = cutaway.storeys[k].clone();
                        if cutaway.avatar_storey == Some(k) {
                            label.push_str("  ●");
                        }
                        let current = k == cutaway.storey;
                        let button = egui::Button::selectable(
                            current,
                            egui::RichText::new(label).size(theme::SIZE_BODY),
                        )
                        .min_size(egui::vec2(WIDTH, theme::row_height()));
                        if ui.add(button).clicked() && !current {
                            pick = Some(Some(k));
                        }
                    }
                    if cutaway.picked.is_some() && cutaway.avatar_storey.is_some() {
                        ui.add_space(2.0);
                        if theme::secondary_button(ui, "", "Follow me", WIDTH).clicked() {
                            pick = Some(None);
                        }
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(if cutaway.designed {
                            "Drawn by its owner"
                        } else {
                            "Generated from the register"
                        })
                        .size(theme::SIZE_TINY)
                        .color(theme::p().text_dim),
                    );
                    if ui
                        .add_enabled_ui(open_url_supported(), |ui| {
                            theme::secondary_button(ui, icons::OPEN_EXTERNAL, "Floor plan", WIDTH)
                        })
                        .inner
                        .on_hover_text("Open this building in the floor-plan editor")
                        .clicked()
                    {
                        plan = true;
                    }
                });
            });
    }
    if plan {
        open = facts
            .building(&id)
            .map(|f| crate::systems::building_card::floor_plan_url(&id, &f.footprint));
    }

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
