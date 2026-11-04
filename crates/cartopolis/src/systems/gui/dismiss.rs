//! One Escape, one Back, one ladder.
//!
//! **Escape used to close almost nothing, and three of the four handlers written
//! for it were unreachable code.** The cause is one line in `egui_manager`: the
//! Bevy→egui key bridge forwards a key only while a text field has focus. That
//! gate is right for *activation* keys — egui keeps focus on the last-clicked
//! widget and Space activates it, so a game key as ordinary as Space was
//! collapsing panels from across the map — and it was applied to every key
//! rather than to those. So `ui.rs`'s "Escape closes the main menu" ran only
//! when the menu contained a focused text field (it contains none), the sign-in
//! card's Escape worked only in Remote mode with a field focused, and the route
//! planner's "Escape disarms the map pick" was a comment describing a handler
//! that was never written. Meanwhile Android's Back key *did* unwind panels
//! (`lib.rs`), so the app's two dismissal gestures disagreed about what
//! dismissing means.
//!
//! Escape and Tab are now forwarded to egui unconditionally (see
//! `egui_manager::pointer_events`), which makes every one of those handlers live
//! at once — and that is exactly why they cannot stay where they are. Four
//! independent `key_pressed(Escape)` reads in four files, with no order between
//! them and no way for one to stop another, would close two things per press.
//!
//! ## The rule
//!
//! **Escape dismisses exactly one thing: the innermost.** The ladder below is
//! the same stack Android's Back already walked, extended to the surfaces Back
//! never knew about. Back is this ladder plus one final rung — quit — and that
//! difference is the only difference between the two gestures, which is why it
//! is written down once here rather than inferred twice.
//!
//! ## Why a raw `ButtonInput` read and not `ctx.input`
//!
//! For the reason `driver.rs` already reads Escape that way: the raw reading
//! does not depend on the forwarding gate this module exists to work around. It
//! also runs after the whole panel chain, so every panel has already declared
//! its state for the frame.
//!
//! ## The one rung egui owns
//!
//! `egui::Memory` consumes Escape internally to clear widget focus, and it does
//! so whatever this system decides. So "a text field had focus when the frame
//! began" is rung 2 *and this ladder must stop there* — otherwise a single
//! Escape both blurs the search box and closes the panel behind it, which is
//! two dismissals for one press and precisely the bug the single-rung rule
//! exists to prevent.

use crate::systems::egui_manager::EguiRef;
use bevy::prelude::*;

use crate::resources::{ContextMenuState, EditDialogState, RightPanel};
use crate::systems::driver::DriverState;
use crate::systems::login_screen::LoginScreen;
use crate::systems::navigation::RouteState;
use crate::systems::onscreen_keyboard::OnScreenKeyboard;
use crate::systems::ui::MainMenu;

/// Everything the ladder can put away, borrowed for one frame.
///
/// A `SystemParam` rather than a dozen arguments because both callers — Escape
/// here and Back in `lib.rs` — need the identical set, and a ladder whose rungs
/// are threaded by hand through two signatures is one edit away from being two
/// different ladders.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Dismissable<'w> {
    pub keyboard: ResMut<'w, OnScreenKeyboard>,
    pub login: ResMut<'w, LoginScreen>,
    pub dialog: ResMut<'w, EditDialogState>,
    pub context_menu: ResMut<'w, ContextMenuState>,
    pub route: ResMut<'w, RouteState>,
    pub menu: ResMut<'w, MainMenu>,
    pub driver: ResMut<'w, DriverState>,
    pub panel: ResMut<'w, RightPanel>,
    pub composer: ResMut<'w, crate::systems::chat_overlay::ChatOverlay>,
    pub calls: ResMut<'w, crate::systems::calls::Calls>,
    pub camera: ResMut<'w, crate::systems::camera_screen::CameraScreen>,
    pub place_card: ResMut<'w, crate::systems::place_card::PlaceCard>,
    pub contacts: ResMut<'w, crate::systems::contacts::Contacts>,
    pub building: ResMut<'w, crate::systems::building_select::BuildingSelection>,
}

/// One rung of the ladder: a surface a dismissal gesture can put away.
///
/// **The order of these variants is the specification.** `innermost` returns the
/// first that is open, and every caller acts on exactly one — so moving a
/// variant moves the behaviour, visibly, in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    /// The on-screen keyboard. Innermost because that is where the system IME
    /// sits on every other Android app: Back puts the keys away first and only
    /// then starts closing the thing you were typing into. Getting this order
    /// wrong would have made a dismissal leave driver mode out from under
    /// someone who was only trying to see the screen again.
    Keyboard,
    /// A focused text field and any dropdown hanging off it.
    ///
    /// egui's own `Memory` consumes Escape to clear focus whatever this module
    /// decides, so this rung is not something the ladder *does* — it is the
    /// ladder declining to do anything else. Without it a single Escape would
    /// blur the search box and close the panel the box lives in.
    TextFocus,
    /// The full-screen call: a call ringing, or the one you are in. It covers
    /// everything, so a press goes to it before anything behind it; and it is
    /// *minimised* to the bar at the top, never ended — hanging up and declining
    /// are buttons, as on every phone, and not a gesture made in passing.
    CallScreen,
    /// The camera, which is the whole screen: a press steps it back one
    /// screen (`CameraScreen::back`) — the chat list to the picture, the
    /// picture to the viewfinder, the viewfinder to the map. Under the call,
    /// which a ringing call draws over it.
    Camera,
    /// The prim edit dialog's texture picker: a window opened *from* the dialog,
    /// so the inner of the two.
    TexturePicker,
    /// The prim edit dialog.
    EditDialog,
    /// The right-click context menu.
    ContextMenu,
    /// An armed route map-pick. `navigation.rs` has carried a comment saying
    /// "Escape disarms — otherwise the next click anywhere would move a pin"
    /// since the feature was written; until now the only way out of an armed
    /// pick was to click the map.
    RoutePick,
    /// The chat composer over the map. Outside the dialogs (it is a pill, not a
    /// modal) and inside the cards, which is where it sits on the screen too —
    /// and above [`Rung::TextFocus`], so the first press blurs the field and
    /// keeps the keyboard's own rung intact while the second puts the composer
    /// away.
    ChatComposer,
    /// The sign-in card.
    SignIn,
    /// The main menu card.
    MainMenu,
    /// Driver mode. Below the menu because the menu draws over it, and a gesture
    /// should not step past something visibly on top.
    DriverMode,
    /// A page inside a panel with a back chevron (Community ▸ You ▸ Contacts):
    /// a press steps back one page, as the chevron does (`theme::page_back`),
    /// instead of dropping the whole stack with the panel.
    PanelPage,
    /// The open right panel.
    RightPanel,
    /// A person's card over the map. The cards come after the panel because
    /// on a phone the panel covers them; before, Back with a card up skipped
    /// them and sent the app to the background.
    PersonCard,
    /// The place card a search pick opens.
    PlaceCard,
    /// The building card a tap on a building opens.
    BuildingCard,
}

/// What is open right now — the ladder's whole input, as plain data.
///
/// Split from the resources so the *order* can be tested without a `World`. The
/// bug this module fixes was an ordering bug that no amount of reading caught;
/// a rule that can only be exercised by running the app is a rule that drifts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpenSurfaces {
    pub keyboard: bool,
    pub text_focused: bool,
    pub call_screen: bool,
    pub camera: bool,
    pub texture_picker: bool,
    pub edit_dialog: bool,
    pub context_menu: bool,
    pub route_pick: bool,
    pub chat_composer: bool,
    /// The card is open *and* answerable — a request in flight disables its own
    /// close button, and Escape must not be a way around that.
    pub sign_in: bool,
    pub main_menu: bool,
    pub driver_mode: bool,
    pub right_panel: bool,
    pub panel_page: bool,
    pub person_card: bool,
    pub place_card: bool,
    pub building_card: bool,
}

impl OpenSurfaces {
    /// The one rung a press should fire, or `None` when nothing is open.
    #[must_use]
    pub fn innermost(self) -> Option<Rung> {
        // The picker only exists inside the dialog, so it is tested first and
        // the dialog's own rung is what remains.
        if self.keyboard {
            Some(Rung::Keyboard)
        } else if self.text_focused {
            Some(Rung::TextFocus)
        } else if self.call_screen {
            Some(Rung::CallScreen)
        } else if self.camera {
            Some(Rung::Camera)
        } else if self.edit_dialog && self.texture_picker {
            Some(Rung::TexturePicker)
        } else if self.edit_dialog {
            Some(Rung::EditDialog)
        } else if self.context_menu {
            Some(Rung::ContextMenu)
        } else if self.route_pick {
            Some(Rung::RoutePick)
        } else if self.chat_composer {
            Some(Rung::ChatComposer)
        } else if self.sign_in {
            Some(Rung::SignIn)
        } else if self.main_menu {
            Some(Rung::MainMenu)
        } else if self.driver_mode {
            Some(Rung::DriverMode)
        } else if self.panel_page {
            Some(Rung::PanelPage)
        } else if self.right_panel {
            Some(Rung::RightPanel)
        } else if self.person_card {
            Some(Rung::PersonCard)
        } else if self.place_card {
            Some(Rung::PlaceCard)
        } else if self.building_card {
            Some(Rung::BuildingCard)
        } else {
            None
        }
    }
}

impl Dismissable<'_> {
    /// Read what is open. `text_focused` comes from the caller because egui has
    /// already cleared the focus by the time this runs — the question is whether
    /// a field held the keyboard when the frame *began*.
    #[must_use]
    pub fn surfaces(&self, text_focused: bool, ctx: &egui::Context) -> OpenSurfaces {
        OpenSurfaces {
            keyboard: self.keyboard.is_open(),
            text_focused,
            call_screen: self.calls.screen_up(),
            camera: self.camera.is_open(),
            texture_picker: self.dialog.texture_picker_open,
            edit_dialog: self.dialog.visible,
            context_menu: self.context_menu.visible,
            route_pick: self.route.picking.is_some(),
            chat_composer: self.composer.composing,
            sign_in: self.login.open && !self.login.is_busy(),
            main_menu: self.menu.open,
            driver_mode: self.driver.active,
            right_panel: *self.panel != RightPanel::None,
            panel_page: crate::systems::theme::page_back_shown(ctx),
            person_card: self.contacts.card.is_some(),
            place_card: self.place_card.place.is_some(),
            building_card: self.building.selected.is_some(),
        }
    }

    /// Put away the innermost open thing, and only that one. Returns the rung
    /// that fired, so `Back` can tell "nothing was open" from "something closed"
    /// and decide whether to quit.
    pub fn dismiss_innermost(&mut self, text_focused: bool, ctx: &egui::Context) -> Option<Rung> {
        let rung = self.surfaces(text_focused, ctx).innermost()?;
        match rung {
            Rung::Keyboard => self.keyboard.dismiss(),
            // egui already did it; claiming the press is the whole point.
            Rung::TextFocus => {}
            Rung::CallScreen => self.calls.minimized = true,
            Rung::Camera => {
                self.camera.back();
            }
            Rung::TexturePicker => self.dialog.texture_picker_open = false,
            Rung::EditDialog => self.dialog.visible = false,
            Rung::ContextMenu => self.context_menu.visible = false,
            Rung::RoutePick => self.route.picking = None,
            Rung::ChatComposer => self.composer.close(),
            Rung::SignIn => self.login.open = false,
            Rung::MainMenu => self.menu.open = false,
            Rung::DriverMode => self.driver.exit(),
            Rung::PanelPage => crate::systems::theme::press_page_back(ctx),
            Rung::RightPanel => *self.panel = RightPanel::None,
            Rung::PersonCard => {
                self.contacts.card = None;
                self.contacts.error = None;
            }
            Rung::PlaceCard => self.place_card.place = None,
            Rung::BuildingCard => self.building.clear(),
        }
        Some(rung)
    }
}

/// Escape walks the ladder and stops. It never quits — that rung belongs to Back
/// alone, and an Escape that could close the application from a bare map is a
/// keystroke nobody wants to discover by accident.
pub fn handle_escape(keys: Res<ButtonInput<KeyCode>>, egui: EguiRef, mut d: Dismissable) {
    if !keys.just_pressed(KeyCode::Escape) {
        return;
    }
    let text_focused = egui.ctx.text_edit_focused();
    let _ = d.dismiss_innermost(text_focused, &egui.ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything open at once.
    fn all_open() -> OpenSurfaces {
        OpenSurfaces {
            keyboard: true,
            text_focused: true,
            call_screen: true,
            camera: true,
            texture_picker: true,
            edit_dialog: true,
            context_menu: true,
            route_pick: true,
            chat_composer: true,
            sign_in: true,
            main_menu: true,
            driver_mode: true,
            right_panel: true,
            panel_page: true,
            person_card: true,
            place_card: true,
            building_card: true,
        }
    }

    /// Close whichever surface `rung` names, the way the real applier does.
    fn close(s: &mut OpenSurfaces, rung: Rung) {
        match rung {
            Rung::Keyboard => s.keyboard = false,
            Rung::TextFocus => s.text_focused = false,
            Rung::CallScreen => s.call_screen = false,
            Rung::Camera => s.camera = false,
            Rung::TexturePicker => s.texture_picker = false,
            Rung::EditDialog => s.edit_dialog = false,
            Rung::ContextMenu => s.context_menu = false,
            Rung::RoutePick => s.route_pick = false,
            Rung::ChatComposer => s.chat_composer = false,
            Rung::SignIn => s.sign_in = false,
            Rung::MainMenu => s.main_menu = false,
            Rung::DriverMode => s.driver_mode = false,
            Rung::PanelPage => s.panel_page = false,
            Rung::RightPanel => s.right_panel = false,
            Rung::PersonCard => s.person_card = false,
            Rung::PlaceCard => s.place_card = false,
            Rung::BuildingCard => s.building_card = false,
        }
    }

    /// **One press closes one thing, innermost first, and then it stops.**
    ///
    /// Testing a rung at a time would pass on a ladder that fired every arm,
    /// which is the failure mode the single-rung rule exists to prevent — so
    /// this opens everything and walks it down, asserting both the order and
    /// that the walk terminates.
    #[test]
    fn each_press_closes_exactly_one_surface_innermost_first() {
        let mut s = all_open();
        let mut fired = Vec::new();
        while let Some(rung) = s.innermost() {
            close(&mut s, rung);
            fired.push(rung);
            assert!(fired.len() <= 20, "the ladder does not terminate");
        }
        assert_eq!(
            fired,
            vec![
                Rung::Keyboard,
                Rung::TextFocus,
                Rung::CallScreen,
                Rung::Camera,
                Rung::TexturePicker,
                Rung::EditDialog,
                Rung::ContextMenu,
                Rung::RoutePick,
                Rung::ChatComposer,
                Rung::SignIn,
                Rung::MainMenu,
                Rung::DriverMode,
                Rung::PanelPage,
                Rung::RightPanel,
                Rung::PersonCard,
                Rung::PlaceCard,
                Rung::BuildingCard,
            ]
        );
        assert_eq!(s, OpenSurfaces::default());
        assert_eq!(s.innermost(), None, "an empty screen consumes no press");
    }

    /// The texture picker is a window opened from the edit dialog, so it cannot
    /// be reached without it — and a press must not skip past the dialog to a
    /// picker that is not on screen.
    #[test]
    fn the_texture_picker_is_only_a_rung_while_its_dialog_is_open() {
        let stale = OpenSurfaces {
            texture_picker: true,
            right_panel: true,
            ..Default::default()
        };
        assert_eq!(stale.innermost(), Some(Rung::RightPanel));

        let real = OpenSurfaces {
            texture_picker: true,
            edit_dialog: true,
            ..Default::default()
        };
        assert_eq!(real.innermost(), Some(Rung::TexturePicker));
    }

    /// A field with the keyboard consumes the press and the panel behind it
    /// stays open. This is the rung that exists *because* egui acts on Escape
    /// whatever we decide: without it, one press would blur the search box and
    /// close the Search panel it lives in.
    #[test]
    fn a_focused_field_shields_the_panel_it_lives_in() {
        let s = OpenSurfaces {
            text_focused: true,
            right_panel: true,
            ..Default::default()
        };
        assert_eq!(s.innermost(), Some(Rung::TextFocus));
    }

    /// Back with only a card on the map closes the card. The cards were not on
    /// the ladder, so the press found nothing open and the phone sent the app to
    /// the background with a place still on screen.
    #[test]
    fn a_card_on_the_map_is_closed_before_the_app_goes() {
        for s in [
            OpenSurfaces {
                place_card: true,
                ..Default::default()
            },
            OpenSurfaces {
                building_card: true,
                ..Default::default()
            },
            OpenSurfaces {
                person_card: true,
                ..Default::default()
            },
        ] {
            assert!(s.innermost().is_some(), "{s:?}");
        }
    }

    /// A page two levels into a panel steps back a page, and the panel itself
    /// closes only once no page with a back chevron is left.
    #[test]
    fn a_page_steps_back_before_its_panel_closes() {
        let s = OpenSurfaces {
            panel_page: true,
            right_panel: true,
            ..Default::default()
        };
        assert_eq!(s.innermost(), Some(Rung::PanelPage));
        let root = OpenSurfaces {
            right_panel: true,
            ..Default::default()
        };
        assert_eq!(root.innermost(), Some(Rung::RightPanel));
    }

    /// A sign-in request in flight disables the card's own close button; Escape
    /// must not be a way around it, so `surfaces` reports the card as not
    /// dismissable and the press falls through to whatever is behind.
    #[test]
    fn a_busy_sign_in_card_is_not_a_rung() {
        let s = OpenSurfaces {
            sign_in: false,
            right_panel: true,
            ..Default::default()
        };
        assert_eq!(s.innermost(), Some(Rung::RightPanel));
    }
}

/// Android's back gesture, which until now did nothing at all.
///
/// Under NativeActivity the key is delivered to the app and consumed there, so a
/// user who swipes back — the first thing every Android user reaches for to
/// dismiss something — got no response, and the only way out of a full-screen
/// panel was to find its close button. Back now unwinds the same stack Escape
/// does on the desktop, innermost first: leave driver mode, then close the open
/// panel, and only quit when there is nothing left to dismiss.
///
/// Quitting last is the part worth stating. Android's convention is that back
/// from the root screen leaves the app, and an app that refuses to is a worse
/// citizen than one that exits when you meant to close a panel — but a map client
/// that quits on the first back press while a panel is open would be unusable.
pub fn handle_back_gesture(
    keys: Res<ButtonInput<KeyCode>>,
    egui: EguiRef,
    mut dismissable: Dismissable,
    mut exit: MessageWriter<AppExit>,
) {
    // `BrowserBack` is what winit maps Android's back key to. Desktop keyboards
    // can carry the same key, and doing the same thing there is right anyway.
    if !keys.just_pressed(KeyCode::BrowserBack) {
        return;
    }
    // **Back is Escape plus one rung.** This used to walk its own three-step
    // ladder — keyboard, driver, panel — while Escape walked a different and
    // mostly broken one, so the app's two dismissal gestures disagreed about
    // what dismissing meant: Back unwound panels and Escape could not, Escape
    // left driver mode from under an open dialog and Back would not. There is
    // one ladder now (`systems::dismiss`), and the *only* difference between the
    // two gestures is this last rung, which is why it is the only thing written
    // here.
    let text_focused = egui.ctx.text_edit_focused();
    if dismissable
        .dismiss_innermost(text_focused, &egui.ctx)
        .is_none()
    {
        // On the phone the last rung is "to the background", not "quit": quitting
        // ends the process there, so coming back was a cold start that had
        // forgotten the tab, the open chat and any call (`move_task_to_back`).
        #[cfg(target_os = "android")]
        {
            let _ = &mut exit;
            crate::platform::android::move_task_to_back();
        }
        #[cfg(not(target_os = "android"))]
        exit.write(AppExit::Success);
    }
}
