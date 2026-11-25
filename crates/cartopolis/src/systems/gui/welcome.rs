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

#[cfg(feature = "comms")]
use crate::resources::{AppPrefs, RightPanel};
#[cfg(feature = "comms")]
use crate::systems::egui_manager::EguiRef;
#[cfg(feature = "comms")]
use crate::systems::login_screen::LoginScreen;
#[cfg(feature = "comms")]
use crate::systems::session::SessionState;
#[cfg(feature = "comms")]
use crate::systems::theme;

#[cfg(feature = "comms")]
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
    /// The component library draws the card (`systems::lib_welcome`): egui's
    /// system then draws nothing, but still decides whether it shows
    /// ([`Self::lib_view`]) and answers what it hands back
    /// ([`Self::lib_action`]).
    pub lib: bool,
    pub lib_view: WelcomeView,
    pub lib_action: Option<WelcomeAction>,
}

/// What the card shows, published for the library's edition.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct WelcomeView {
    pub shown: bool,
    /// The server signs people up on its web page ("Create account").
    pub register: bool,
}

/// One of the card's three answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WelcomeAction {
    SignIn,
    CreateAccount,
    NotNow,
}

/// An answer, in either edition: sign in, sign up through the browser (and
/// come back to the card that takes the account), or just the map.
#[cfg(feature = "comms")]
fn answer(
    action: WelcomeAction,
    welcome: &mut Welcome,
    prefs: &mut AppPrefs,
    login: &mut LoginScreen,
) {
    match action {
        WelcomeAction::SignIn => login.open = true,
        WelcomeAction::CreateAccount => {
            if let Some(url) = login.register_url() {
                crate::systems::data_layers::open_url(&url);
            }
            // Back from the browser with an account, the card that takes it
            // is already open.
            login.open = true;
        }
        WelcomeAction::NotNow => welcome.community = false,
    }
    if welcome.capture.is_none() {
        prefs.welcome_seen = true;
    }
}

/// The card, while it has not been answered. The app's only (`comms`): in
/// the web build signing in is the page's around the World tab.
#[cfg(feature = "comms")]
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
    // An answer the library's card handed back.
    if let Some(action) = welcome.lib_action {
        welcome.lib_action = None;
        answer(action, &mut welcome, &mut prefs, &mut login);
    }
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
    let visible = show
        && !(session.is_online() || session.status.is_busy() || login.open)
        && !(driver.active || keyboard.is_open() || menu.open || !right_panel.is(RightPanel::None))
        // A card the person opened is what they are looking at.
        && !(place.place.is_some() || building.selected.is_some());
    if welcome.lib {
        let view = WelcomeView {
            shown: visible,
            register: visible && login.register_url().is_some(),
        };
        if welcome.lib_view != view {
            welcome.lib_view = view;
        }
        return;
    }
    if !visible {
        return;
    }

    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let ctx = egui.ctx();
    let mut answer_with = None;
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
                        answer_with = Some(WelcomeAction::SignIn);
                    }
                    if register.is_some()
                        && theme::secondary_button(ui, "", "Create account", half).clicked()
                    {
                        answer_with = Some(WelcomeAction::CreateAccount);
                    }
                });
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    if theme::ghost_button(ui, "", "Not now, just the map").clicked() {
                        answer_with = Some(WelcomeAction::NotNow);
                    }
                });
            });
        });
    if let Some(action) = answer_with {
        answer(action, &mut welcome, &mut prefs, &mut login);
    }
}
