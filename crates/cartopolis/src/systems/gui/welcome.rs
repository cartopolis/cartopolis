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
//! The component library draws it (`systems::lib_welcome`) above the dock;
//! [`drive_welcome`] decides whether it shows, suppressed by everything that
//! owns the bottom of the screen, and gives its answers.

use bevy::prelude::*;

#[cfg(feature = "comms")]
use crate::resources::{AppPrefs, RightPanel};
#[cfg(feature = "comms")]
use crate::systems::login_screen::LoginScreen;
#[cfg(feature = "comms")]
use crate::systems::session::SessionState;

/// What overrides the saved answer, for a scripted capture: `Some(true)` shows
/// the card (`--ui welcome`), `Some(false)` keeps it off every other shot,
/// which runs with no `user.json` and so would otherwise show it on all of them.
#[derive(Resource, Default)]
pub struct Welcome {
    pub capture: Option<bool>,
    /// Community was chosen signed out (`dock::drive_dock`): this card is
    /// what Community is without an account (`docs/mockups/redesign`,
    /// v2-account, A1), whether or not it has been answered before. Signing
    /// in opens the Community page; "Not now" and World go back to the map.
    pub community: bool,
    /// Whether the card shows, for the library's edition
    /// (`systems::lib_welcome`), which hands its answer back in
    /// [`Self::lib_action`].
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

/// An answer: sign in, sign up through the browser (and
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

/// Whether the card shows, published for the library, and the answer it
/// handed back. The app's only (`comms`): in the web build signing in is the
/// page's around the World tab.
#[cfg(feature = "comms")]
#[allow(clippy::too_many_arguments)]
pub fn drive_welcome(
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
    let view = WelcomeView {
        shown: visible,
        register: visible && login.register_url().is_some(),
    };
    if welcome.lib_view != view {
        welcome.lib_view = view;
    }
}
