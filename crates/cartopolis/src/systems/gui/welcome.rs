//! The card a first launch shows over the map: what signing in is for.
//!
//! **A fresh install was a silent offline map.** Nothing on it said that
//! people, chats or communities exist; the way in was Menu ▸ Sign in…, and the
//! only explanation of it was a hover tooltip, which a phone never shows
//! (`docs/mockups/ux-review`, problem 1). This card says it once, with the two
//! ways in and a way to just look at the map, and then stays away:
//! [`AppPrefs::welcome_seen`] is set by any of its three buttons, and by
//! signing in at all.
//!
//! A floating `Area` above the dock, on the place card's terms and outside the
//! panel chain for the same reason (`map-chrome.md`), and suppressed by
//! everything that owns the bottom of the screen.

use bevy::prelude::*;

use crate::resources::{AppPrefs, RightPanel};
use crate::systems::egui_manager::EguiRef;
use crate::systems::login_screen::LoginScreen;
use crate::systems::session::SessionState;
use crate::systems::theme;

/// Width of the card, fixed for the reason [`crate::systems::place_card`]'s is:
/// an anchored, size-to-content `Area` grows a little every frame otherwise.
const CARD_WIDTH: f32 = 300.0;

/// What overrides the saved answer, for a scripted capture: `Some(true)` shows
/// the card (`--ui welcome`), `Some(false)` keeps it off every other shot,
/// which runs with no `user.json` and so would otherwise show it on all of them.
#[derive(Resource, Default)]
pub struct Welcome {
    pub capture: Option<bool>,
    /// Community was chosen signed out (`dock::render_dock`): this card is
    /// what Community is without an account (`docs/mockups/redesign`,
    /// v2-account, A1), whether or not it has been answered before. Signing
    /// in opens the Community page; "Not now" and World go back to the map.
    pub community: bool,
}

/// The card, while it has not been answered.
#[allow(clippy::too_many_arguments)]
pub fn render_welcome(
    egui: EguiRef,
    mut welcome: ResMut<Welcome>,
    mut prefs: ResMut<AppPrefs>,
    mut login: ResMut<LoginScreen>,
    session: Res<SessionState>,
    mut right_panel: ResMut<RightPanel>,
    menu: Res<crate::systems::ui::MainMenu>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    place: Res<crate::systems::place_card::PlaceCard>,
    building: Res<crate::systems::building_select::BuildingSelection>,
) {
    // Somebody who has signed in knows what it is for, and a remembered
    // session coming up on launch is that somebody.
    if session.is_online() && !prefs.welcome_seen {
        prefs.welcome_seen = true;
    }
    // Signed in from Community: on to the Community page it was asked for.
    if session.is_online() && welcome.community {
        welcome.community = false;
        if right_panel.is(RightPanel::None) {
            *right_panel = RightPanel::Chat;
        }
    }
    let show = welcome.community || welcome.capture.unwrap_or(!prefs.welcome_seen);
    if !show || session.is_online() || session.status.is_busy() || login.open {
        return;
    }
    if driver.active || keyboard.is_open() || menu.open || !right_panel.is(RightPanel::None) {
        return;
    }
    // A card the person opened is what they are looking at.
    if place.place.is_some() || building.selected.is_some() {
        return;
    }

    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let ctx = egui.ctx();
    let mut answered = false;
    let mut not_now = false;
    egui::Area::new(egui::Id::new("welcome_card"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let width = CARD_WIDTH.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN);
                ui.set_width(width);
                ui.label(
                    egui::RichText::new("Cartopolis is better with people")
                        .size(theme::SIZE_TITLE)
                        .color(theme::p().cream),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "Sign in to see who is around, chat, and join the communities \
                         near you.",
                    )
                    .size(theme::SIZE_SUB)
                    .color(theme::p().text_dim),
                );
                ui.add_space(10.0);
                let register = login.register_url();
                ui.horizontal(|ui| {
                    let half = if register.is_some() {
                        (width - ui.spacing().item_spacing.x) / 2.0
                    } else {
                        width
                    };
                    if theme::primary_button_sized(ui, "Sign in", half).clicked() {
                        login.open = true;
                        answered = true;
                    }
                    if let Some(url) = &register {
                        let resp = ui.add_sized(
                            egui::vec2(half, theme::PRIMARY_BUTTON_HEIGHT),
                            egui::Button::new("Create account"),
                        );
                        if resp.clicked() {
                            crate::systems::data_layers::open_url(url);
                            // Back from the browser with an account, the card
                            // that takes it is already open.
                            login.open = true;
                            answered = true;
                        }
                    }
                });
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Not now, just the map")
                                    .size(theme::SIZE_SUB)
                                    .color(theme::p().text_dim),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        answered = true;
                        not_now = true;
                    }
                });
            });
        });
    if answered && welcome.capture.is_none() {
        prefs.welcome_seen = true;
    }
    if not_now {
        welcome.community = false;
    }
}
