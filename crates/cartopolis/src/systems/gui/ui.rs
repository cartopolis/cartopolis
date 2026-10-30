//! Prim UI system (Phase 5 - ADR-017).
//!
//! Renders context menus and edit dialogs using egui.

use crate::components::{NeedsMeshRebuild, Prim, PrimShape, Selected};
use crate::resources::OnlineSession;
use crate::resources::RightPanel;
use crate::resources::{
    ContextMenuState, EditDialogState, GameState, PrimTextureCache, TextureLibrary,
};
use crate::systems::egui_manager::EguiMut;
use crate::systems::icons;
use crate::systems::theme;
use bevy::pbr::StandardMaterial;
use bevy::prelude::*;
use cartopolis_core::NetMessage;
use egui::Window;

/// The main menu's state: whether the card above the dock is showing.
///
/// A resource rather than a `Local` because two systems share it — the dock
/// paints the mark disc that toggles it (and shows it pressed while it is open),
/// and [`render_main_menu`] paints the card. `toggled` is that hand-off: without
/// it the click that opens the menu is also a click *outside* the card, which the
/// dismiss rule below would act on in the same frame, and the menu would never
/// appear.
#[derive(Resource, Default)]
pub struct MainMenu {
    pub open: bool,
    /// Set by the dock on the frame the disc was clicked; consumed here.
    pub toggled: bool,
}

/// Width of the menu card, in points. Fixed rather than sized to its content for
/// the reason the driver bar's module note gives: this is an anchored,
/// size-to-content `Area`, and anything inside it that reaches for the
/// *available* width would grow the card a little every frame.
const MENU_WIDTH: f32 = 232.0;

/// The app menu's account entry (`systems::session`), bundled into one parameter
/// because [`render_main_menu`] would otherwise sit at Bevy's 16-parameter
/// ceiling — the same reason `network::AuthWriteback` exists.
///
/// `session` is a `ResMut` because **Log out happens here**, not on the sign-in
/// card: it is one entry with one outcome, and routing it through a modal that
/// then has to be dismissed is three taps for a verb that needs one.
#[derive(bevy::ecs::system::SystemParam)]
pub struct SignInMenu<'w> {
    screen: ResMut<'w, crate::systems::login_screen::LoginScreen>,
    session: ResMut<'w, crate::systems::session::SessionState>,
    identity: Res<'w, crate::resources::AuthIdentity>,
}

/// The main menu: a card that rises out of the dock's mark disc, holding
/// everything that is not a place you navigate to.
///
/// **This is what is left of the address bar.** That was a card in the top-left
/// corner: a mark you clicked to unfold a search field, three history buttons, the
/// camera picker and a hamburger. It began as a full-width strip, then a card that
/// revealed itself when the pointer grazed the top edge (undiscoverable — you
/// cannot aim at something you have never seen — and fired on every trip to the
/// window's title bar), then the folding card. All three put the two controls
/// reached most often, search and the menu, in the corner furthest from the thumb
/// of anyone holding a phone, and all three had to fold half their contents into
/// the menu on a narrow viewport anyway. So the search field is a panel
/// (`geo_nav::render_search_panel`), the mark is a dock disc, and what the compact
/// layout folded away *permanently* lives here — one menu on every viewport,
/// rather than a menu whose contents depend on the window width.
///
/// Anchored to the bottom centre rather than to the disc it belongs to: the dock
/// is centred and its width depends on whether there is a session (Say comes and
/// goes), so an offset measured from the disc would move under a card that should
/// not move. It is a floating `Area`, so like the dock and the HUD it is
/// deliberately *not* in the panel chain.
#[allow(clippy::too_many_arguments)]
pub fn render_main_menu(
    mut egui: EguiMut,
    mut menu: ResMut<MainMenu>,
    mut dev_state: ResMut<crate::resources::DevPanelState>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    mut app_exit: MessageWriter<AppExit>,
    mut sign_in: SignInMenu,
    // Written here, read and cleared by `diagnostics::share_diagnostics`.
    mut diagnostics: ResMut<crate::systems::diagnostics::DiagnosticsRequest>,
) {
    let dismissed_by_disc = std::mem::take(&mut menu.toggled);
    if !menu.open {
        return;
    }

    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let max_height = (egui.viewport_pts().y - bottom - 24.0).max(120.0);
    let ctx = egui.ctx_mut().clone();
    let mut close = false;
    let mut log_out = false;

    let area = egui::Area::new(egui::Id::new("main_menu"))
        .order(egui::Order::Foreground)
        // **The sizing pass decides how tall this card may ever be.** An `Area`
        // lays out its first frame against `Spacing::default_area_size`, which
        // egui defaults to 600×**400**, then remembers the content's size and
        // uses *that* as the `max_rect` for every frame after. Overflow is
        // supposed to grow it back — "if the contents overflow, the area will
        // grow" — but only if the content reports its true height, and this one
        // cannot: the `ScrollArea` shrinks to the room it is given, and inside a
        // justified top-down layout the widgets past the bottom are allocated
        // whatever is left, which by then is a few points. The result is a
        // *stable* wrong answer — content and container agree at ~340 pt — so it
        // does not flicker or recover, it just ends.
        //
        // What that cost: the card stopped after About, and Sign in…, the status
        // readout, Developer tools and Exit were simply not on screen. Every one
        // of them is the only route to what it does. It reads as a menu that is
        // merely short, which is why it survived a UI sweep that was looking at
        // this very card.
        //
        // So the sizing pass gets the whole band the card is allowed, and the
        // remembered size is then the content's own.
        .default_size(egui::vec2(MENU_WIDTH, max_height))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(&ctx, |ui| {
            // `Frame::popup`, not `theme::overlay_frame`: the shared overlay frame
            // is translucent, which is right for a HUD readout and wrong under a
            // list of commands over a moving map.
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(MENU_WIDTH);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().button_padding.y = 4.0;
                        // The brand, at the top of the card the mark opened — the
                        // same two-tone wordmark the splash and the sign-in card
                        // wear (`theme::WORDMARK_HEAD`).
                        theme::wordmark_ui(ui, theme::SIZE_TITLE);
                        ui.separator();

                        // Justified, the way egui lays a menu out: every entry
                        // takes the card's full width, so the hit targets line up
                        // and a two-word label does not make a button half the
                        // size of its neighbour. The entries themselves are
                        // `theme::menu_entry` — a line of a list, not a plate;
                        // see that helper for what a card of full-width buttons
                        // looked like.
                        ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                            // **The account is the first entry**, because it is the
                            // one thing in this card that changes what the app *is*
                            // rather than what it shows — and because signed out,
                            // it is the only thing here anybody is looking for.
                            //
                            // Signed in it is a single verb. It used to read
                            // "jeroen · cartopolis.org" and open the sign-in card
                            // again, where a Sign out button sat beside a Sign in
                            // one: a modal, to leave.
                            if sign_in.session.is_online() {
                                let who = match sign_in.identity.username.as_deref() {
                                    Some(user) => {
                                        format!(
                                            "Signed in as {user} · {}",
                                            sign_in.session.mode.where_()
                                        )
                                    }
                                    None => {
                                        format!("Signed in to {}", sign_in.session.mode.where_())
                                    }
                                };
                                if theme::menu_entry(ui, icons::LOGOUT, "Log out")
                                    .on_hover_text(who)
                                    .clicked()
                                {
                                    log_out = true;
                                    close = true;
                                }
                            } else if theme::menu_entry(ui, icons::KEY, "Sign in…")
                                .on_hover_text(
                                    "Share a world with other people on a simulator you can reach",
                                )
                                .clicked()
                            {
                                sign_in.screen.open = true;
                                close = true;
                            }
                            // **Why a session failure is reported here.** The
                            // sign-in card used to raise itself whenever one
                            // arrived — a modal the app pushed in front of you,
                            // mid-flight, for a question you had not asked. It
                            // opens from the entry above and from nowhere else
                            // now, so the reason a remembered session did not
                            // come up, or a live one dropped, lands next to the
                            // entry that offers to try again. Losing it entirely
                            // would leave a client that silently fell back to
                            // the offline map with nothing anywhere saying why.
                            if let Some(error) = &sign_in.session.error {
                                theme::note(ui, error);
                            }
                            ui.separator();

                            // Panels you *visit* rather than flip: Appearance is a
                            // wardrobe, so it does not earn a permanent disc in the
                            // dock next to the places you actually navigate between.
                            //
                            // Nearby chat joined them when chatting stopped being a
                            // panel at all (`systems::chat_overlay`): the dock's Say
                            // disc raises the composer over the map, and what is left
                            // in here is the log, the rooms, the blocks and the
                            // microphone. Only in a session — there is nobody to have
                            // said anything otherwise.
                            for panel in [
                                // **Appearance is session-bound too**, and that is
                                // the one of the three that costs something: an
                                // offline avatar is visible and is wearing
                                // whatever `user.json` last remembered, so this
                                // hides the only way to change a body you can
                                // still see. It is the right trade anyway. An
                                // appearance is a thing other people look at —
                                // that is what it is *for* — and offline there is
                                // nobody to look, so what the panel offers alone
                                // on the map is a wardrobe with no occasion. The
                                // saved choice is not lost; it comes back with
                                // the session that gives it a point.
                                crate::resources::RightPanel::Appearance,
                                crate::resources::RightPanel::Chat,
                                // Models is a cupboard you go to, not a place you
                                // navigate between — same argument as Appearance
                                // above. Only in a session: the inventory belongs
                                // to an account on a server, and offline there is
                                // neither.
                                crate::resources::RightPanel::Models,
                                crate::resources::RightPanel::Settings,
                                // About carries the attribution the map data's licences
                                // require, so it must be reachable from the running app,
                                // not only from a file in the tarball.
                                crate::resources::RightPanel::About,
                            ]
                            .into_iter()
                            .filter(|p| {
                                !matches!(
                                    p,
                                    crate::resources::RightPanel::Appearance
                                        | crate::resources::RightPanel::Chat
                                        | crate::resources::RightPanel::Models
                                ) || sign_in.session.is_online()
                            }) {
                                // Same glyph the panel's own header wears, so the menu
                                // entry and the badge it opens are recognisably one thing.
                                if theme::menu_entry_selected(
                                    ui,
                                    right_panel.is(panel),
                                    panel.icon(),
                                    panel.label(),
                                )
                                .clicked()
                                {
                                    right_panel.toggle(panel);
                                    close = true;
                                }
                            }
                            ui.separator();
                            // The inspection surface. It was two entries — this
                            // one and a "Status readout" that put time, position
                            // and altitude in a box over the top-right of the map
                            // — and the readout is gone (`systems::hud`'s module
                            // note): the position and altitude are two taps into
                            // this panel, where somebody asking where the camera
                            // is already is, and the sun and weather chips it also
                            // carried are in Settings ▸ World.
                            //
                            // It carries the accent when it is open rather than
                            // renaming itself to "Hide …": a toggle that is lit
                            // says the same thing in a word less, and it is the
                            // state the panel entries above already show.
                            if theme::menu_entry_selected(
                                ui,
                                dev_state.open,
                                icons::TUNE,
                                "Developer tools",
                            )
                            .clicked()
                            {
                                dev_state.open = !dev_state.open;
                                close = true;
                            }
                            // Next to the developer tools because it is one — but
                            // above the fold rather than inside the dev panel,
                            // because the person who needs it is holding a phone
                            // and reporting a bug, not inspecting the renderer.
                            // It is the only way anything a device knows about
                            // itself gets off that device short of a crash
                            // (`systems::diagnostics`).
                            if theme::menu_entry(ui, icons::COPY, "Share diagnostics…")
                                .on_hover_text(
                                    "The build, the GPU, the render tier and what the \
                                     avatar is doing, plus this run's log — to the share \
                                     sheet on a phone, to the clipboard everywhere else",
                                )
                                .clicked()
                            {
                                diagnostics.pending = true;
                            }
                            if let Some(result) = diagnostics.last.as_deref() {
                                theme::note(ui, result);
                            }
                            ui.separator();
                            if theme::menu_entry(ui, icons::CLEAR, "Exit").clicked() {
                                app_exit.write(AppExit::Success);
                                close = true;
                            }
                        });
                    });
            });
        });

    if log_out {
        crate::systems::session::sign_out(&mut sign_in.session, &mut store);
    }
    // Dismiss on a click that landed outside the card — but never on the one that
    // opened it, which is a click outside the card by definition (see
    // `MainMenu::toggled`). Escape closes it too, the way every other transient
    // surface in this app does.
    let clicked_out = ctx.input(|i| {
        i.pointer.any_click()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !area.response.rect.contains(p))
    });
    // Escape is not read here. It closed nothing from this site anyway — the key
    // reached egui only while a text field had focus, and this card contains no
    // field — and now that it does reach egui, one handler per surface would
    // close two things per press. `systems::dismiss` owns the whole ladder.
    if close || (clicked_out && !dismissed_by_disc) {
        menu.open = false;
    }
}

// The camera-mode picker (a Follow/Fly/Earth chip row plus a "View whole
// Earth" jump) used to be drawn inline here. It is the mode disc in the
// bottom-right corner of the map now (`map_osd::mode_button`): a camera switch
// is a change to the map and belongs on it, not two taps deep in a card that
// covers it — and the disc, cycling only the modes on offer, is also where
// "Follow needs a signed-in session" is enforced for the UI.

/// The avatar designer (☰ ▸ Appearance): shapes the MakeHuman body with
/// sliders (`avatar_designer`). The avatar wears the draft while the panel is
/// open (`HumanDraft`); Apply commits it to the persisted store, which
/// `human_avatar::rebuild_on_look_change` watches to rebuild the model.
///
/// **The menu only offers it in a session** (`render_main_menu`), because an
/// appearance is a thing other people look at. This closes it on the *transition*
/// out of one rather than refusing to draw whenever there is no session, and the
/// difference is not pedantry: a scripted capture is always offline
/// (`--ui appearance` sets `RightPanel` directly), so a flat "online or nothing"
/// rule would mean the wardrobe is the one panel no README image can ever show.
/// What is actually wrong is a panel left standing after a sign-out, and that is
/// an edge.
pub fn render_avatar_chooser(
    mut egui: EguiMut,
    mut chooser: ResMut<crate::resources::AvatarChooserState>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
    mut right_panel: ResMut<RightPanel>,
    session: Res<crate::systems::session::SessionState>,
    mut was_online: Local<bool>,
    // The MakeHuman designer: the draft the avatar is rebuilt from while it is
    // open, and its tab.
    mut human_draft: ResMut<crate::systems::human_avatar::HumanDraft>,
    mut designer: Local<crate::systems::avatar_designer::DesignerState>,
    mut camera: ResMut<crate::resources::CameraState>,
    mut designer_turn: ResMut<crate::systems::human_avatar::DesignerTurn>,
) {
    let online = session.is_online();
    if std::mem::replace(&mut was_online, online) && !online {
        // Closed if it is open, never toggled: a toggle *opened* the wardrobe
        // whenever a session ended with something else on screen — and every
        // phone resume ends one (the socket dies while suspended), so the app
        // came back to Appearance instead of where it was.
        if right_panel.is(RightPanel::Appearance) {
            *right_panel = RightPanel::None;
        }
        chooser.seeded = false;
    }
    // Seed the draft from the saved appearance the first time the panel opens.
    if right_panel.is(RightPanel::Appearance) && !chooser.seeded {
        chooser.draft = store.data.appearance.clone();
        if chooser.draft.human.is_none() {
            chooser.draft.human = Some(crate::systems::human_avatar::current_look(
                &chooser.draft,
                &crate::systems::human_avatar::HumanDraft::default(),
            ));
        }
        chooser.seeded = true;
    }
    // Borrowed, not cloned: this ran five String clones per frame whether the
    // panel was open or not, and the closure only compares against it — the
    // store write on Apply happens after the closure ends.
    let saved = &store.data.appearance;
    let mut apply = false;
    // On a phone the designer is a sheet across the bottom half: a panel that
    // filled the screen would hide the avatar it is shaping.
    let sheet = egui.compact();
    let body = |ui: &mut egui::Ui| {
        // Outside the scroll area, so the header and the close button stay put
        // while the body scrolls under them — and so the body is what is
        // reachable on a 609 pt viewport, where these rows overrun the screen.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let d = &mut chooser.draft;
                if let Some(look) = d.human.as_mut() {
                    crate::systems::avatar_designer::designer(
                        ui,
                        &mut designer,
                        look,
                        saved.human.as_ref(),
                    );
                    ui.separator();
                    let changed = *d != *saved;
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(changed, egui::Button::new("Apply"))
                            .clicked()
                        {
                            apply = true;
                        }
                        if !changed {
                            ui.weak("This is your current avatar.");
                        }
                    });
                }
            });
    };
    let closed = if sheet {
        crate::systems::egui_manager::bottom_sheet(
            &mut egui,
            &mut right_panel,
            RightPanel::Appearance,
            "appearance_sheet",
            0.5,
            body,
        )
    } else {
        crate::systems::egui_manager::right_panel(
            &mut egui,
            &mut right_panel,
            RightPanel::Appearance,
            "appearance_panel",
            280.0,
            body,
        )
    };
    if apply {
        store.data.appearance = chooser.draft.clone();
        store.dirty = true;
    }
    if closed {
        chooser.seeded = false;
    }
    // While the designer is open the avatar wears the draft; closing it, saved
    // or not, puts it back in the saved look. Compared first, so an idle
    // panel does not mark the resource changed every frame.
    let showing = (right_panel.is(RightPanel::Appearance) && chooser.seeded)
        .then(|| chooser.draft.human.clone())
        .flatten();
    // On a phone, aim the camera near the feet while the sheet is up, so
    // the whole body stands in the half of the screen the sheet leaves.
    // (-0.3 m: at the default follow distance the head clears the top and the
    // feet the sheet; 0.25 put the legs under it.)
    // On the Face tab, come close and aim at the face instead: its controls
    // move things millimetres across, unreadable from the follow distance.
    // (1.0 m to the head at 1.62 m: the whole head and the shoulders, on a
    // phone's top half too — the boom is above the sheet's edge.)
    let close = showing.is_some() && designer.close_up();
    let aim = if close {
        Some(1.62)
    } else {
        (sheet && showing.is_some()).then_some(-0.3)
    };
    if camera.aim_height != aim {
        camera.aim_height = aim;
    }
    let reach = close.then_some(1.0);
    if camera.aim_reach != reach {
        camera.aim_reach = reach;
    }
    let turn = if showing.is_some() {
        designer.view.turn()
    } else {
        0.0
    };
    if designer_turn.0 != turn {
        designer_turn.0 = turn;
    }
    if human_draft.0 != showing {
        human_draft.0 = showing;
    }
}

/// Fires the pending chrome-free capture (a chat's snapshot, a polaroid, the
/// camera's world). Runs in Update **after** the panels that ask for one, so the
/// click is seen this frame; because `suppress_egui_for_screenshot` drops the
/// egui output later this same frame (while `hide_ui` is set), the captured
/// swapchain has no UI chrome. After the capturing frame, `hide_ui` is cleared
/// and the panels reappear.
pub fn drive_screenshots(
    mut commands: Commands,
    mut shot: ResMut<crate::resources::ScreenshotState>,
) {
    use bevy::render::view::screenshot::Screenshot;

    // A clip's picture (`camera_screen::recorder`): the window as it is, UI and
    // all, and nothing hidden, so it goes beside any other capture and does
    // not return.
    #[cfg(not(target_arch = "wasm32"))]
    if std::mem::take(&mut shot.camera_frame) {
        commands.spawn(Screenshot::primary_window()).observe(
            |captured: On<bevy::render::view::screenshot::ScreenshotCaptured>,
             mut camera: ResMut<crate::systems::camera_screen::CameraScreen>| {
                camera.frame_in =
                    crate::systems::camera_screen::recorder::Grab::from_image(&captured.image);
            },
        );
    }
    if std::mem::take(&mut shot.post_snapshot) {
        // A public post's polaroid (`community_posts`): the same chrome-free
        // capture as a chat's snapshot, its pixels handed to the post being
        // written, which frames it as a polaroid on a worker.
        shot.hide_ui = true;
        commands.spawn(Screenshot::primary_window()).observe(
            |captured: On<bevy::render::view::screenshot::ScreenshotCaptured>,
             mut cs: ResMut<crate::systems::communities::Communities>| {
                let image = &captured.image;
                let size = image.size();
                match image.clone().try_into_dynamic() {
                    Ok(dynamic) => {
                        cs.posts.snapshot_in =
                            Some((dynamic.to_rgba8().into_raw(), [size.x, size.y]));
                    }
                    Err(e) => {
                        cs.posts.snapshot_busy = false;
                        warn!("a polaroid snapshot could not be read: {e}");
                    }
                }
            },
        );
        return;
    }
    if std::mem::take(&mut shot.camera_snapshot) {
        // The camera's world: the window with no UI, for a polaroid or, in
        // Mixed, from the pose the camera is holding the view on, handed back
        // as pixels to be put beside the photograph.
        shot.hide_ui = true;
        commands.spawn(Screenshot::primary_window()).observe(
            |captured: On<bevy::render::view::screenshot::ScreenshotCaptured>,
             mut check: ResMut<crate::systems::camera_screen::CameraScreen>| {
                let image = &captured.image;
                let size = image.size();
                match image.clone().try_into_dynamic() {
                    Ok(dynamic) => {
                        check.world_in = Some((dynamic.to_rgba8().into_raw(), [size.x, size.y]));
                    }
                    Err(e) => warn!("the camera's world could not be read: {e}"),
                }
            },
        );
        return;
    }
    if std::mem::take(&mut shot.face_snapshot) {
        // A polaroid for your own picture: the window with no UI, cropped by
        // `faces` to the square its viewfinder showed.
        shot.hide_ui = true;
        commands.spawn(Screenshot::primary_window()).observe(
            |captured: On<bevy::render::view::screenshot::ScreenshotCaptured>,
             mut faces: ResMut<crate::systems::faces::Faces>| {
                let image = &captured.image;
                let size = image.size();
                match image.clone().try_into_dynamic() {
                    Ok(dynamic) => {
                        faces.snapshot_in = Some((
                            dynamic.to_rgba8().into_raw(),
                            [size.x as usize, size.y as usize],
                        ));
                    }
                    Err(e) => {
                        faces.shooting = false;
                        faces.note = Some("The polaroid could not be read.".into());
                        warn!("a face polaroid could not be read: {e}");
                    }
                }
            },
        );
        return;
    }
    if let Some(channel) = shot.chat_snapshot.take() {
        // The pixels go to the chat as raw RGBA: encoding a full window here
        // would stall the frame, and `chat_pictures` re-encodes on a worker
        // anyway. `hide_ui` stays set for this frame and is cleared on the next.
        shot.hide_ui = true;
        commands.spawn(Screenshot::primary_window()).observe(
            move |captured: On<bevy::render::view::screenshot::ScreenshotCaptured>,
                  mut chat: ResMut<crate::systems::chat::ChatState>| {
                let image = &captured.image;
                let size = image.size();
                match image.clone().try_into_dynamic() {
                    Ok(dynamic) => {
                        chat.private_pictures
                            .push(crate::systems::chat_pictures::OutgoingPicture {
                                channel: channel.clone(),
                                bytes: dynamic.to_rgba8().into_raw(),
                                caption: String::new(),
                                rgba: Some([size.x, size.y]),
                            })
                    }
                    Err(e) => warn!("a chat snapshot could not be read: {e}"),
                }
            },
        );
        return;
    }
    // The frame after a capture: the UI comes back.
    if shot.hide_ui {
        shot.hide_ui = false;
    }
}

/// When a screenshot is being captured, drop this frame's tessellated egui geometry so
/// the shot is free of UI chrome. Runs in PostUpdate **after** `end_egui_frame` (which
/// writes the output) and before render-world extraction. Only `clipped_primitives` is
/// cleared — `textures_delta` is left intact so egui's font/texture uploads aren't lost.
pub fn suppress_egui_for_screenshot(
    shot: Res<crate::resources::ScreenshotState>,
    mut output: ResMut<crate::systems::egui_manager::EguiRenderOutput>,
) {
    if shot.hide_ui {
        output.clipped_primitives.clear();
        // This mutates the output in place, so `end_egui_frame` must rewrite it next
        // frame even if egui reports nothing changed — otherwise the cleared geometry
        // is what stays resident and the UI never comes back after a screenshot.
        output.force_next_write = true;
        output.generation = output.generation.wrapping_add(1);
    }
}

/// Fill the edit dialog's fields and revert snapshots from a prim. Leaves `visible` and
/// `active_tab` untouched, so it serves both opening the dialog and following a selection
/// change. Sets `is_new = false` and `prim_id` to this prim.
pub fn populate_edit_dialog(edit_dialog: &mut EditDialogState, prim: &Prim, transform: &Transform) {
    edit_dialog.prim_id = Some(prim.id);
    edit_dialog.is_new = false;
    edit_dialog.name = prim.name.clone();
    let color = {
        let c = prim.color.to_linear();
        [c.red, c.green, c.blue]
    };
    edit_dialog.color = color;
    let shape = format!("{:?}", prim.shape).to_lowercase();
    edit_dialog.shape = shape.clone();
    let pos = transform.translation;
    edit_dialog.position = [pos.x, pos.y, pos.z];
    let rot = transform.rotation.to_euler(EulerRot::XYZ);
    edit_dialog.rotation = [rot.0, rot.1, rot.2];
    let scale_v = transform.scale;
    edit_dialog.scale = [scale_v.x, scale_v.y, scale_v.z];
    edit_dialog.texture_id = prim.texture_id.clone();
    edit_dialog.model = prim.model.clone();
    edit_dialog.path_cut_begin = prim.path_cut_begin;
    edit_dialog.path_cut_end = prim.path_cut_end;
    edit_dialog.hollow = prim.hollow;
    edit_dialog.surface = prim.surface;
    edit_dialog.repeats_per_meter = 0.0;
    edit_dialog.warp = crate::resources::PrimWarp {
        twist_begin: prim.twist_begin,
        twist_end: prim.twist_end,
        taper_x: prim.taper_x,
        taper_y: prim.taper_y,
        top_shear_x: prim.top_shear_x,
        top_shear_y: prim.top_shear_y,
        slice_begin: prim.slice_begin,
        slice_end: prim.slice_end,
    };
    // Snapshot for Cancel revert.
    edit_dialog.original_name = prim.name.clone();
    edit_dialog.original_color = color;
    edit_dialog.original_shape = shape;
    edit_dialog.original_position = edit_dialog.position;
    edit_dialog.original_rotation = edit_dialog.rotation;
    edit_dialog.original_scale = edit_dialog.scale;
    edit_dialog.original_texture_id = prim.texture_id.clone();
    edit_dialog.original_model = prim.model.clone();
    edit_dialog.playing_video = prim.video.is_some();
    edit_dialog.video_note = None;
    edit_dialog.physical = prim.physical;
    edit_dialog.physical_request = None;
    edit_dialog.original_path_cut_begin = prim.path_cut_begin;
    edit_dialog.original_path_cut_end = prim.path_cut_end;
    edit_dialog.original_hollow = prim.hollow;
    edit_dialog.original_warp = edit_dialog.warp;
    edit_dialog.original_surface = prim.surface;
    edit_dialog.texture_picker_open = false;
}

/// While the edit dialog is open, keep it pointed at the currently selected prim so
/// clicking a different prim updates the dialog instead of leaving it on the old one.
pub fn sync_dialog_to_selection(
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    prim_query: Query<(&Prim, &Transform)>,
) {
    // Only follow selection for an open editor of an existing prim (never clobber a
    // half-filled "Create object" dialog).
    if !edit_dialog.visible || edit_dialog.is_new {
        return;
    }
    let Some(selected) = game_state.selected_prim_id else {
        return;
    };
    if edit_dialog.prim_id == Some(selected) {
        return; // already showing the selected prim
    }
    for (prim, transform) in prim_query.iter() {
        if prim.id == selected {
            populate_edit_dialog(&mut edit_dialog, prim, transform);
            game_state.editing_prim_id = Some(selected);
            break;
        }
    }
}

/// The map's context menu: right-click on a desktop, **long-press on a phone**
/// (`systems::touch` fires an emulated `Right` for a held finger, which is what
/// `picking` opens this on).
///
/// It is a ring of round buttons around the point (`systems::action_ring`,
/// `docs/mockups/layers-and-ring/v1.png`), with a pill under it saying what was
/// pointed at. It is reached from a point on the map rather than from a panel, so
/// it is where the acts that take a *coordinate* live: route to or from here,
/// save or share here as a place, build here. On an object the ring is the
/// object's instead: touch it, edit it, script it, delete it, follow it.
///
/// At 16 system parameters this is at Bevy's cap. The next thing that needs world
/// access here should be grouped into a `SystemParam` rather than added to the
/// list — `camera_controls` hit the same wall and that is what it did.
pub fn render_context_menu(
    mut commands: Commands,
    mut egui: EguiMut,
    mut context_menu: ResMut<ContextMenuState>,
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    prim_query: Query<(Entity, &Prim, &Transform)>,
    selected_query: Query<Entity, With<Selected>>,
    mut route: ResMut<crate::systems::navigation::RouteState>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    mut script_editor: ResMut<crate::systems::script_editor::ScriptEditorState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    // Saving the point under the cursor as a place. `BookmarksState` is `Res` and
    // not `ResMut` on purpose: `request_address` resolves the name through an
    // interior-mutability channel, so this menu does not have to contend with the
    // Places panel for the resource every frame it happens to be open.
    mut store: ResMut<crate::systems::user_store::UserStore>,
    bookmarks: Res<crate::systems::bookmarks::BookmarksState>,
    camera_state: Res<crate::resources::CameraState>,
    camera: crate::grid::AnchoredCamera,
    mut attach: ResMut<crate::systems::camera_attach::CameraAttach>,
) {
    use crate::resources::RightPanel;
    use crate::systems::action_ring::{self, RingAction, RingOutcome};
    use crate::systems::navigation::Endpoint;

    if !context_menu.visible {
        return;
    }

    // Read before anything borrows the world: this is one number.
    let camera_altitude = camera.world_position_f64().map(|world| world.y);
    let prim = context_menu.hit_prim_id.and_then(|id| {
        prim_query
            .iter()
            .find(|(_, p, _)| p.id == id)
            .map(|(_, p, _)| (id, p.name.clone()))
    });
    let lat_lng = context_menu.lat_lng;

    // The actions, primary first. No key hints in the labels: `E` is *interact*,
    // `D` strafes right, and a phone reaches this with no keyboard at all.
    let mut actions: Vec<RingAction> = Vec::new();
    if prim.is_some() {
        // Scripts are server state, so Touch and Script are online-only —
        // `--offline` has no one to store a script or fire a touch.
        if online.is_some() {
            actions.push(RingAction {
                key: "touch",
                icon: icons::TOUCH,
                label: "Touch",
                hint: "Fire the object's on_touch handler",
            });
        }
        actions.push(RingAction {
            key: "edit",
            icon: icons::RENAME,
            label: "Edit",
            hint: "Edit this object",
        });
        if online.is_some() {
            actions.push(RingAction {
                key: "script",
                icon: icons::NOTE,
                label: "Script",
                hint: "Write a Luau script that runs on this object",
            });
        }
        actions.push(RingAction {
            key: "follow",
            icon: icons::FOLLOW,
            label: "Follow",
            hint: "Keep the camera on this object as it moves",
        });
        actions.push(RingAction {
            key: "delete",
            icon: icons::DELETE,
            label: "Delete",
            hint: "Delete this object",
        });
    } else if lat_lng.is_some() {
        // "Route to here" is the default act — a point on a map is a place you
        // want to *go*. "Route from here" is the quieter of the two: needing it
        // means a journey that does not start where you are.
        actions.push(RingAction {
            key: "route_to",
            icon: icons::ROUTE,
            label: "Route here",
            hint: "Set this point as the destination",
        });
        // A post is public and lives on a server: online only.
        if online.is_some() {
            actions.push(RingAction {
                key: "post_here",
                icon: icons::NOTE,
                label: "Post here",
                hint: "Write a public post about this spot, to a community",
            });
        }
        // What the municipality has published around here (`systems::plans`),
        // with or without the layer on — only where the feed has anything.
        if lat_lng.is_some_and(|(lat, lng)| crate::systems::plans::covers(lat, lng)) {
            actions.push(RingAction {
                key: "plans_here",
                icon: icons::FLAG,
                label: "Plans here",
                hint: "What the municipality has published about this spot",
            });
        }
        if camera_altitude.is_some() {
            actions.push(RingAction {
                key: "save",
                icon: icons::BOOKMARK_ADD,
                label: "Save",
                hint: "Add this point to Places, named by reverse lookup",
            });
        }
        actions.push(RingAction {
            key: "share",
            icon: icons::SHARE,
            label: "Share",
            hint: "Share a link that opens the map here",
        });
        // Gated on the *session*, which is what "can I build here?" means: a
        // prim is created by `CreatePrim` and lives in the simulator.
        if online.is_some() {
            actions.push(RingAction {
                key: "build",
                icon: icons::BUILD,
                label: "Build",
                hint: "Create an object here",
            });
        }
        actions.push(RingAction {
            key: "route_from",
            icon: icons::START,
            label: "Route from",
            hint: "Set this point as the starting point",
        });
    }
    if actions.is_empty() {
        context_menu.visible = false;
        return;
    }

    let (title, detail) = match (&prim, lat_lng) {
        (Some((_, name)), _) => (name.clone(), "Object".to_string()),
        (None, Some((lat, lng))) => ("This spot".to_string(), format!("{lat:.5}, {lng:.5}")),
        (None, None) => (String::new(), String::new()),
    };
    // A new id per opening, so the press that opened it is not read as a press
    // off the ring (`action_ring::show`).
    let at = egui.window_to_ui(context_menu.screen_pos);
    let id = egui::Id::new("map_ring").with((
        context_menu.screen_pos.x.to_bits(),
        context_menu.screen_pos.y.to_bits(),
    ));
    let outcome = action_ring::show(egui.ctx_mut(), id, at, &title, &detail, &actions);

    let chosen = match outcome {
        RingOutcome::Open => return,
        RingOutcome::Dismissed => {
            context_menu.visible = false;
            return;
        }
        RingOutcome::Chosen(key) => key,
    };
    context_menu.visible = false;

    match (chosen, lat_lng, &prim) {
        ("route_to", Some((lat, lng)), _) => {
            route.set_endpoint_at(Endpoint::To, lat, lng);
            // The start fills itself in: opening the panel runs
            // `RouteState::default_start`, which is where "here" is defined.
            *right_panel = RightPanel::Route;
        }
        ("route_from", Some((lat, lng)), _) => {
            route.set_endpoint_at(Endpoint::From, lat, lng);
            *right_panel = RightPanel::Route;
        }
        ("post_here", Some(at), _) => context_menu.post_here = Some(at),
        ("plans_here", Some(at), _) => context_menu.plans_here = Some(at),
        ("save", Some((lat, lng)), _) => {
            // Altitude and pose come from the camera rather than from the hit
            // point, because a saved place is a remembered *view* as well as a
            // coordinate (`CameraPose::apply`).
            if let Some(altitude) = camera_altitude {
                let id = store.add_bookmark(
                    crate::systems::bookmarks::PLACEHOLDER_NAME.to_string(),
                    lat,
                    lng,
                    altitude,
                    crate::systems::geo_nav::CameraPose::capture(&camera_state),
                );
                bookmarks.request_address(id, lat, lng);
                *right_panel = RightPanel::Places;
            }
        }
        ("share", Some((lat, lng)), _) => {
            let alt = camera_altitude.unwrap_or(300.0).max(50.0);
            let link = crate::systems::place_link::web_link(lat, lng, alt, None);
            crate::systems::contacts::share(&link);
        }
        ("build", _, _) => {
            edit_dialog.prim_id = None;
            edit_dialog.is_new = true;
            edit_dialog.name = "Object".to_string();
            edit_dialog.color = [0.5, 0.5, 0.5]; // neutral gray
            edit_dialog.shape = "box".to_string();
            edit_dialog.texture_id = None;
            edit_dialog.model = None;
            edit_dialog.texture_picker_open = false;
            edit_dialog.path_cut_begin = 0.0;
            edit_dialog.path_cut_end = 1.0;
            edit_dialog.hollow = 0.0;
            edit_dialog.warp = crate::resources::PrimWarp::default();
            edit_dialog.surface = cartopolis_core::PrimSurface::default();
            edit_dialog.repeats_per_meter = 0.0;
            let hit = context_menu.hit_point;
            // Offset Y so the prim rests on the surface instead of being centred in it.
            edit_dialog.position = [hit.x, hit.y + 0.5, hit.z];
            edit_dialog.rotation = [0.0, 0.0, 0.0];
            edit_dialog.scale = [1.0, 1.0, 1.0];
            edit_dialog.visible = true;
            game_state.editing_prim_id = None;
        }
        ("edit", _, Some((prim_id, _))) => {
            for entity in selected_query.iter() {
                commands.entity(entity).remove::<Selected>();
            }
            for (entity, p, transform) in prim_query.iter() {
                if p.id == *prim_id {
                    commands.entity(entity).insert(Selected);
                    game_state.selected_prim_id = Some(*prim_id);
                    game_state.editing_prim_id = Some(*prim_id);
                    populate_edit_dialog(&mut edit_dialog, p, transform);
                    edit_dialog.visible = true;
                    break;
                }
            }
        }
        ("delete", _, Some((prim_id, _))) => {
            tracing::info!(prim_id, "deleting prim");
            game_state.prims_to_delete.push(*prim_id);
        }
        ("script", _, Some((prim_id, _))) => {
            script_editor.open(*prim_id);
            *right_panel = RightPanel::Script;
        }
        ("touch", _, Some((prim_id, _))) => {
            if let Some(session) = online.as_ref() {
                session.send(
                    "touch prim",
                    cartopolis_core::NetMessage::TouchPrim {
                        request_id: 0,
                        prim_id: *prim_id,
                    },
                );
            }
        }
        ("follow", _, Some((prim_id, name))) => {
            attach.attach(
                crate::systems::prim_geo::prim_attach_key(*prim_id),
                name.clone(),
                crate::systems::prim_geo::PRIM_ATTACH_RANGE,
            );
        }
        _ => {}
    }
}

/// Width of the object editor and its texture picker, clamped to the viewport.
///
/// The floor is deliberately below anything comfortable: on a viewport narrower
/// than the floor the dialog is cut off either way, and a width that goes
/// negative is a panic in egui's layout rather than an ugly dialog.
fn dialog_width(viewport_w: f32, preferred: f32) -> f32 {
    preferred.min(viewport_w - 16.0).max(200.0)
}

/// How much of a phone's screen height the object editor may cover. The rest is
/// the object itself, which is the thing being edited.
const SHEET_HEIGHT: f32 = 0.55;

/// One labelled X/Y/Z row: the label on its own line, then three fields sharing
/// the width equally.
///
/// It was one row of a four-column grid, label first, and the label alone is
/// ~120 pt — so on a phone the Z field landed past the right-hand edge of the
/// dialog and of the screen. Returns which of the three changed.
fn xyz_fields(
    ui: &mut egui::Ui,
    label: &str,
    values: [&mut f32; 3],
    speed: f64,
    suffix: &str,
    min: Option<f32>,
) -> [bool; 3] {
    ui.label(label);
    let gap = ui.spacing().item_spacing.x;
    let w = ((ui.available_width() - 2.0 * gap) / 3.0).max(40.0);
    let h = ui.spacing().interact_size.y;
    let mut changed = [false; 3];
    ui.horizontal(|ui| {
        for ((value, axis), out) in values.into_iter().zip(["X ", "Y ", "Z "]).zip(&mut changed) {
            let mut field = egui::DragValue::new(value)
                .prefix(axis)
                .suffix(suffix)
                .speed(speed);
            if let Some(min) = min {
                field = field.range(min..=f32::MAX);
            }
            *out = ui.add_sized([w, h], field).changed();
        }
    });
    changed
}

/// The frame the object editor and the texture picker wear: the sign-in card's
/// recipe, which is what a themed window looks like in this app — a glass fill,
/// a lit rim, the shared corner radius and real margins.
fn dialog_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::window(style)
        .fill(theme::GLASS)
        .stroke(egui::Stroke::new(1.0, theme::OVERLAY_LINE))
        .inner_margin(egui::Margin::symmetric(10, 10))
}

/// The object (prim) editor.
///
/// **It wears the panels' chrome, not egui's.** This is the primary surface of
/// the building tool — right-click anything in the world and it opens — and it
/// was the one window still carrying a stock title bar, collapse triangle and
/// close glyph, against nine right-hand panels wearing `theme::panel_header`'s
/// raised band, accent medallion and painted cross. The recipe is the sign-in
/// card's: `title_bar(false)`, a themed frame, and the shared header for the
/// title and the way out.
pub fn render_edit_dialog(
    mut egui: EguiMut,
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    texture_lib: Res<TextureLibrary>,
    mut texture_cache: ResMut<PrimTextureCache>,
    mut gizmo_state: ResMut<crate::systems::gizmo::GizmoState>,
) {
    if !edit_dialog.visible {
        return;
    }

    // 320 pt of dialog does not fit a 274 pt phone viewport, and a `Window` is no
    // help there: it clamps its *position* to the screen, not its size. The Shape
    // tab is a good 700 pt tall on its own, so the height needs the same treatment.
    //
    // On a phone the dialog is a sheet along the bottom of the screen: full
    // width, fixed in place, and no taller than `SHEET_HEIGHT` of the screen, so
    // the object being edited stays in view above it. Left to itself the window
    // opened wherever egui put it and hung off the right-hand edge.
    let viewport = egui.viewport_pts();
    let compact = egui.compact();
    let width = dialog_width(viewport.x, 320.0);
    let max_height = if compact {
        (viewport.y * SHEET_HEIGHT).max(200.0)
    } else {
        (viewport.y - 24.0).max(200.0)
    };
    let ctx = egui.ctx_mut();
    let mut dialog_open = true;

    let title = if edit_dialog.is_new {
        "Create object"
    } else {
        "Edit object"
    };

    let mut window = Window::new(title)
        .id(egui::Id::new("prim_edit_dialog"))
        .title_bar(false)
        .default_width(width)
        .min_width(width)
        .max_width(width)
        .max_height(max_height)
        .frame(dialog_frame(&ctx.style_of(ctx.theme())));
    if compact {
        window = window
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -8.0))
            .resizable(false);
    }
    window.show(ctx, |ui| {
        ui.set_max_width(width);
        if theme::panel_header(ui, icons::TUNE, title) {
            dialog_open = false;
        }
        // Tab bar
        ui.horizontal(|ui| {
            // Not "Edit / Object / Texture": the dialog is *called* "Edit
            // object" now, so two of those three named the dialog rather than
            // the tab. These name what is behind them.
            ui.selectable_value(&mut edit_dialog.active_tab, 0, "Basics");
            ui.selectable_value(&mut edit_dialog.active_tab, 1, "Shape");
            ui.selectable_value(&mut edit_dialog.active_tab, 2, "Surface");
        });
        ui.separator();

        // Only the tab scrolls: the title (and its close cross) and the
        // Save/Cancel row stay put, or a short screen leaves the way out below
        // the fold.
        let spacing = ui.spacing().item_spacing.y;
        let footer = theme::row_height() + 3.0 * spacing + 8.0;
        let body = (max_height - ui.min_rect().height() - footer).max(80.0);
        egui::ScrollArea::vertical()
            .max_height(body)
            .auto_shrink([false, true])
            .show(ui, |ui| match edit_dialog.active_tab {
                0 => basics_tab(ui, &mut edit_dialog, &mut gizmo_state),
                1 => shape_tab(ui, &mut edit_dialog),
                _ => surface_tab(ui, &mut edit_dialog, &texture_lib),
            });

        ui.separator();

        dialog_buttons(ui, &mut edit_dialog, &mut game_state);
    });

    if !dialog_open {
        push_revert(&mut game_state, &edit_dialog);
        edit_dialog.visible = false;
        edit_dialog.texture_picker_open = false;
        game_state.editing_prim_id = None;
    }

    render_texture_picker(
        &mut egui,
        &mut edit_dialog,
        &texture_lib,
        &mut texture_cache,
    );
}

// The tab and section helpers below take the system's `ResMut`s by reference
// rather than the resources themselves, so each one dereferences mutably exactly
// where the code did when it was inline — change detection sees the same writes.

/// The object editor's Basics tab: name, gizmo tool, physics.
fn basics_tab(
    ui: &mut egui::Ui,
    edit_dialog: &mut ResMut<EditDialogState>,
    gizmo_state: &mut ResMut<crate::systems::gizmo::GizmoState>,
) {
    use crate::systems::gizmo::GizmoMode;
    egui::Grid::new("edit_grid")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut edit_dialog.name);
            ui.end_row();

            ui.label("Tool");
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut gizmo_state.mode, GizmoMode::Translate, "Move (T)");
                ui.radio_value(&mut gizmo_state.mode, GizmoMode::Rotate, "Rotate (R)");
                ui.radio_value(&mut gizmo_state.mode, GizmoMode::Scale, "Scale (S)");
            });
            ui.end_row();

            // Physical (protocol 34): its own message, like Video,
            // so it applies at once rather than on Save, and only to
            // a prim the server already has.
            if !edit_dialog.is_new {
                ui.label("Physical");
                let mut on = edit_dialog.physical;
                if crate::systems::theme::checkbox(ui, &mut on, "Falls, bounces and can be pushed")
                    .changed()
                {
                    edit_dialog.physical = on;
                    edit_dialog.physical_request = Some(on);
                }
                ui.end_row();
            }
        });
}

/// The object editor's Shape tab: block type, transform, and the path and
/// warp parameters.
fn shape_tab(ui: &mut egui::Ui, edit_dialog: &mut ResMut<EditDialogState>) {
    egui::Grid::new("object_grid")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Building block type");
            egui::ComboBox::from_id_salt("shape_combo")
                .selected_text(shape_display_name(&edit_dialog.shape))
                .show_ui(ui, |ui| {
                    for (key, label) in SHAPES {
                        ui.selectable_value(&mut edit_dialog.shape, key.to_string(), *label);
                    }
                });
            ui.end_row();
        });

    ui.separator();

    // Position / Size / Rotation
    let [px, py, pz] = &mut edit_dialog.position;
    xyz_fields(ui, "Position (meters)", [px, py, pz], 0.1, "", None);
    let [sx, sy, sz] = &mut edit_dialog.scale;
    xyz_fields(ui, "Size (meters)", [sx, sy, sz], 0.1, "", Some(0.001));
    let mut deg = edit_dialog.rotation.map(f32::to_degrees);
    let [dx, dy, dz] = &mut deg;
    let changed = xyz_fields(ui, "Rotation (degrees)", [dx, dy, dz], 1.0, "°", None);
    for (axis, changed) in changed.into_iter().enumerate() {
        if changed {
            edit_dialog.rotation[axis] = deg[axis].to_radians();
        }
    }

    ui.separator();

    // Path Cut Begin and End
    ui.label("Path cut begin and end");
    ui.horizontal(|ui| {
        let pce = edit_dialog.path_cut_end;
        let pcb = edit_dialog.path_cut_begin;
        ui.label("B");
        ui.add(
            egui::DragValue::new(&mut edit_dialog.path_cut_begin)
                .speed(0.005)
                .range(0.0..=pce)
                .min_decimals(3)
                .max_decimals(3),
        );
        ui.label("E");
        ui.add(
            egui::DragValue::new(&mut edit_dialog.path_cut_end)
                .speed(0.005)
                .range(pcb..=1.0)
                .min_decimals(3)
                .max_decimals(3),
        );
    });

    ui.add_space(4.0);

    // Hollow (stored 0–0.95, displayed 0–95)
    ui.label("Hollow");
    let mut hollow_pct = edit_dialog.hollow * 100.0;
    if ui
        .add(
            egui::DragValue::new(&mut hollow_pct)
                .speed(0.5)
                .range(0.0..=95.0)
                .min_decimals(1)
                .max_decimals(1),
        )
        .changed()
    {
        edit_dialog.hollow = hollow_pct / 100.0;
    }

    ui.add_space(4.0);

    // Twist Begin and End (degrees, −360..360)
    ui.label("Twist begin and end");
    ui.horizontal(|ui| {
        ui.label("B");
        ui.add(
            egui::DragValue::new(&mut edit_dialog.warp.twist_begin)
                .speed(1.0)
                .range(-360.0..=360.0)
                .suffix("°"),
        );
        ui.label("E");
        ui.add(
            egui::DragValue::new(&mut edit_dialog.warp.twist_end)
                .speed(1.0)
                .range(-360.0..=360.0)
                .suffix("°"),
        );
    });

    ui.add_space(4.0);

    // Taper (−1..1 per axis)
    ui.label("Taper");
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut edit_dialog.warp.taper_x)
                .prefix("X ")
                .speed(0.01)
                .range(-1.0..=1.0)
                .max_decimals(2),
        );
        ui.add(
            egui::DragValue::new(&mut edit_dialog.warp.taper_y)
                .prefix("Y ")
                .speed(0.01)
                .range(-1.0..=1.0)
                .max_decimals(2),
        );
    });

    ui.add_space(4.0);

    // Top Shear (−0.5..0.5 per axis)
    ui.label("Top shear");
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut edit_dialog.warp.top_shear_x)
                .prefix("X ")
                .speed(0.01)
                .range(-0.5..=0.5)
                .max_decimals(2),
        );
        ui.add(
            egui::DragValue::new(&mut edit_dialog.warp.top_shear_y)
                .prefix("Y ")
                .speed(0.01)
                .range(-0.5..=0.5)
                .max_decimals(2),
        );
    });

    ui.add_space(4.0);

    // Slice Begin and End (trims the path 0..1, begin ≤ end)
    ui.label("Slice begin and end");
    ui.horizontal(|ui| {
        let sb = edit_dialog.warp.slice_begin;
        let se = edit_dialog.warp.slice_end;
        ui.label("B");
        ui.add(
            egui::DragValue::new(&mut edit_dialog.warp.slice_begin)
                .speed(0.005)
                .range(0.0..=se)
                .min_decimals(3)
                .max_decimals(3),
        );
        ui.label("E");
        ui.add(
            egui::DragValue::new(&mut edit_dialog.warp.slice_end)
                .speed(0.005)
                .range(sb..=1.0)
                .min_decimals(3)
                .max_decimals(3),
        );
    });
}

/// The object editor's Surface tab: colour, video, texture and its mapping.
fn surface_tab(
    ui: &mut egui::Ui,
    edit_dialog: &mut ResMut<EditDialogState>,
    texture_lib: &TextureLibrary,
) {
    egui::Grid::new("texture_grid")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label("Color");
            let mut srgb = [
                (edit_dialog.color[0] * 255.0) as u8,
                (edit_dialog.color[1] * 255.0) as u8,
                (edit_dialog.color[2] * 255.0) as u8,
            ];
            if ui.color_edit_button_srgb(&mut srgb).changed() {
                edit_dialog.color = [
                    srgb[0] as f32 / 255.0,
                    srgb[1] as f32 / 255.0,
                    srgb[2] as f32 / 255.0,
                ];
            }
            ui.end_row();

            // Video (protocol 28): its own message, not part of the
            // save — so only for a prim that already exists, and
            // only where a clip can be chosen and played.
            if !edit_dialog.is_new && crate::systems::map::prim_video_available() {
                ui.label("Video");
                ui.horizontal_wrapped(|ui| {
                    ui.label(if edit_dialog.playing_video {
                        "Playing"
                    } else {
                        "None"
                    });
                    if crate::platform::model_file::is_available()
                        && ui.button("Choose video…").clicked()
                    {
                        edit_dialog.video_action = Some(crate::resources::VideoAction::Choose);
                    }
                    if edit_dialog.playing_video && ui.button("Stop").clicked() {
                        edit_dialog.video_action = Some(crate::resources::VideoAction::Stop);
                    }
                    if let Some(note) = &edit_dialog.video_note {
                        ui.label(
                            egui::RichText::new(note.as_str())
                                .small()
                                .color(crate::systems::theme::TEXT_DIM),
                        );
                    }
                })
                .response
                .on_hover_text(cartopolis_core::video_limits::VIDEO_FORMAT_HINT);
                ui.end_row();
            }

            ui.label("Texture");
            ui.horizontal(|ui| {
                ui.label(edit_dialog.texture_id.as_deref().unwrap_or("None"));
                if !texture_lib.entries.is_empty() && ui.button("Pick…").clicked() {
                    edit_dialog.texture_picker_open = !edit_dialog.texture_picker_open;
                }
                if edit_dialog.texture_id.is_some() && ui.button("Clear").clicked() {
                    edit_dialog.texture_id = None;
                }
            });
            ui.end_row();

            // Transparency (SL "Transparency %": 0 = opaque, higher = more see-through).
            ui.label("Transparency");
            let mut transparency = (1.0 - edit_dialog.surface.alpha) * 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut transparency)
                        .speed(0.5)
                        .range(0.0..=100.0)
                        .suffix(" %")
                        .max_decimals(1),
                )
                .changed()
            {
                edit_dialog.surface.alpha = (1.0 - transparency / 100.0).clamp(0.0, 1.0);
            }
            ui.end_row();

            // Glow (0–1; emissive surface glow).
            ui.label("Glow");
            ui.add(
                egui::DragValue::new(&mut edit_dialog.surface.glow)
                    .speed(0.01)
                    .range(0.0..=1.0)
                    .max_decimals(2),
            );
            ui.end_row();

            ui.label("Full bright");
            crate::systems::theme::checkbox(ui, &mut edit_dialog.surface.full_bright, "");
            ui.end_row();
        });

    ui.separator();

    // Repeats Per Face (tiling count per axis, with optional flip).
    ui.label("Repeats per face");
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::DragValue::new(&mut edit_dialog.surface.repeat_u)
                .prefix("U ")
                .speed(0.05)
                .range(0.0..=100.0)
                .max_decimals(3),
        );
        crate::systems::theme::checkbox(ui, &mut edit_dialog.surface.flip_u, "Flip");
        ui.add(
            egui::DragValue::new(&mut edit_dialog.surface.repeat_v)
                .prefix("V ")
                .speed(0.05)
                .range(0.0..=100.0)
                .max_decimals(3),
        );
        crate::systems::theme::checkbox(ui, &mut edit_dialog.surface.flip_v, "Flip");
    });

    ui.add_space(4.0);

    // Texture rotation (degrees, −360..360).
    ui.label("Rotation (degrees)");
    ui.add(
        egui::DragValue::new(&mut edit_dialog.surface.rotation)
            .speed(1.0)
            .range(-360.0..=360.0)
            .suffix("°"),
    );

    ui.add_space(4.0);

    // Repeats Per Meter: derives per-face repeats from object size on Apply.
    ui.label("Repeats per metre");
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut edit_dialog.repeats_per_meter)
                .speed(0.05)
                .range(0.0..=10.0)
                .max_decimals(3),
        );
        if ui.button("Apply").clicked() && edit_dialog.repeats_per_meter > 0.0 {
            let rpm = edit_dialog.repeats_per_meter;
            edit_dialog.surface.repeat_u = rpm * edit_dialog.scale[0];
            edit_dialog.surface.repeat_v = rpm * edit_dialog.scale[1];
        }
    });

    ui.add_space(4.0);

    // Texture offset (fraction of a repeat).
    ui.label("Offset");
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut edit_dialog.surface.offset_u)
                .prefix("U ")
                .speed(0.005)
                .range(-1.0..=1.0)
                .max_decimals(3),
        );
        ui.add(
            egui::DragValue::new(&mut edit_dialog.surface.offset_v)
                .prefix("V ")
                .speed(0.005)
                .range(-1.0..=1.0)
                .max_decimals(3),
        );
    });
}

/// Save, Cancel and Delete, under every tab.
fn dialog_buttons(
    ui: &mut egui::Ui,
    edit_dialog: &mut ResMut<EditDialogState>,
    mut game_state: &mut ResMut<GameState>,
) {
    // Dialog buttons — always visible regardless of active tab
    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            game_state.pending_prim_save = Some(edit_dialog.clone());
            edit_dialog.visible = false;
            game_state.editing_prim_id = None;
        }

        if ui.button("Cancel").clicked() {
            push_revert(&mut game_state, &edit_dialog);
            edit_dialog.visible = false;
            game_state.editing_prim_id = None;
        }

        if !edit_dialog.is_new && ui.button("Delete").clicked() {
            if let Some(id) = edit_dialog.prim_id {
                game_state.prims_to_delete.push(id);
            }
            edit_dialog.visible = false;
            game_state.editing_prim_id = None;
        }
    });
}

/// The texture library picker the Surface tab opens.
fn render_texture_picker(
    egui: &mut EguiMut,
    edit_dialog: &mut ResMut<EditDialogState>,
    texture_lib: &TextureLibrary,
    texture_cache: &mut ResMut<PrimTextureCache>,
) {
    // Texture picker window — the editor's chrome, for the same reason the editor
    // has it: two windows opened from one another, one themed and one stock, look
    // like two applications.
    if edit_dialog.texture_picker_open && edit_dialog.visible {
        let mut picker_open = edit_dialog.texture_picker_open;
        let mut clicked_texture: Option<String> = None;
        let picker_w = dialog_width(egui.viewport_pts().x, 380.0);
        let picker_h = (egui.viewport_pts().y - 24.0).max(200.0);
        let ctx = egui.ctx_mut();
        Window::new("texture library")
            .id(egui::Id::new("prim_texture_picker"))
            .title_bar(false)
            .default_width(picker_w)
            .max_width(picker_w)
            .max_height(picker_h)
            .frame(dialog_frame(&ctx.style_of(ctx.theme())))
            .show(ctx, |ui| {
                ui.set_max_width(picker_w);
                if theme::panel_header(ui, icons::PALETTE, "Texture library") {
                    picker_open = false;
                }
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let cols = 4usize;
                        egui::Grid::new("tex_grid")
                            .num_columns(cols)
                            .spacing([8.0, 8.0])
                            .show(ui, |ui| {
                                for (i, entry) in texture_lib.entries.iter().enumerate() {
                                    let selected =
                                        edit_dialog.texture_id.as_deref() == Some(&entry.id);
                                    let cell_clicked = ui
                                        .vertical(|ui| {
                                            let clicked = if let Some(handle) =
                                                texture_lib.egui_handles.get(&entry.id)
                                            {
                                                let sized = egui::load::SizedTexture::new(
                                                    handle.id(),
                                                    egui::vec2(72.0, 72.0),
                                                );
                                                let resp = ui.add(
                                                    egui::Image::new(sized)
                                                        .sense(egui::Sense::click()),
                                                );
                                                if selected {
                                                    ui.painter().rect_stroke(
                                                        resp.rect,
                                                        2.0,
                                                        egui::Stroke::new(
                                                            2.5_f32,
                                                            ui.visuals().selection.bg_fill,
                                                        ),
                                                        egui::StrokeKind::Outside,
                                                    );
                                                }
                                                resp.clicked()
                                            } else {
                                                // Fallback text tile while egui handle is pending.
                                                let resp = ui.add_sized(
                                                    [72.0, 72.0],
                                                    egui::Button::new(&entry.name)
                                                        .selected(selected),
                                                );
                                                // Fetched once scrolled into view, so
                                                // opening the picker is not a request
                                                // for the whole catalogue.
                                                if ui.is_rect_visible(resp.rect) {
                                                    texture_cache.want(&entry.id);
                                                }
                                                resp.clicked()
                                            };
                                            ui.label(
                                                egui::RichText::new(&entry.name)
                                                    .small()
                                                    .line_height(Some(14.0)),
                                            );
                                            clicked
                                        })
                                        .inner;
                                    if cell_clicked {
                                        clicked_texture = Some(entry.id.clone());
                                    }
                                    if (i + 1) % cols == 0 {
                                        ui.end_row();
                                    }
                                }
                                if !texture_lib.entries.is_empty()
                                    && texture_lib.entries.len() % cols != 0
                                {
                                    ui.end_row();
                                }
                            });
                    });
            });
        if let Some(id) = clicked_texture {
            edit_dialog.texture_id = Some(id);
            picker_open = false;
        }
        edit_dialog.texture_picker_open = picker_open;
    }
}

/// Sync dialog values → prim entity every frame while the edit dialog is open.
/// This gives live preview; Save persists to DB, Cancel reverts via `pending_prim_revert`.
pub fn apply_live_prim_edits(
    mut commands: Commands,
    edit_dialog: Res<EditDialogState>,
    game_state: Res<GameState>,
    mut prim_query: Query<(
        Entity,
        &mut Prim,
        &mut Transform,
        Option<&MeshMaterial3d<StandardMaterial>>,
        Has<crate::components::OwnedMaterial>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if !edit_dialog.visible || edit_dialog.is_new {
        return;
    }
    let Some(prim_id) = game_state.editing_prim_id else {
        return;
    };
    for (entity, mut prim, mut transform, mat_handle, owned) in prim_query.iter_mut() {
        if prim.id != prim_id {
            continue;
        }
        transform.translation = Vec3::new(
            edit_dialog.position[0],
            edit_dialog.position[1],
            edit_dialog.position[2],
        );
        transform.rotation = Quat::from_euler(
            EulerRot::XYZ,
            edit_dialog.rotation[0],
            edit_dialog.rotation[1],
            edit_dialog.rotation[2],
        );
        transform.scale = Vec3::new(
            edit_dialog.scale[0],
            edit_dialog.scale[1],
            edit_dialog.scale[2],
        );
        let new_color = Color::srgb(
            edit_dialog.color[0],
            edit_dialog.color[1],
            edit_dialog.color[2],
        );
        prim.color = new_color;
        prim.name = edit_dialog.name.clone();

        // Detect geometry changes and request a mesh rebuild.
        let new_shape = PrimShape::from_str(&edit_dialog.shape);
        let geom_changed = new_shape != prim.shape
            || (edit_dialog.path_cut_begin - prim.path_cut_begin).abs() > 0.001
            || (edit_dialog.path_cut_end - prim.path_cut_end).abs() > 0.001
            || (edit_dialog.hollow - prim.hollow).abs() > 0.001
            || warp_differs(&edit_dialog.warp, &prim);

        if geom_changed {
            prim.shape = new_shape;
            prim.path_cut_begin = edit_dialog.path_cut_begin;
            prim.path_cut_end = edit_dialog.path_cut_end;
            prim.hollow = edit_dialog.hollow;
            apply_warp(&mut prim, &edit_dialog.warp);
            commands.entity(entity).insert(NeedsMeshRebuild);
        }

        // Update material directly — Selected doesn't change during editing so
        // highlight_selected_prim won't fire on its own. Copy-on-write out of the
        // shared pool so the live preview only restyles the prim being edited.
        if let Some(handle) = mat_handle {
            if let Some(mut mat) = crate::systems::rendering::owned_material(
                &mut commands,
                &mut materials,
                entity,
                &handle.0,
                owned,
            ) {
                // Live preview of the texture surface (transparency / glow / full-bright / UV),
                // then a subtle selection highlight on top so the surface stays visible.
                crate::systems::rendering::apply_surface(&mut mat, new_color, &edit_dialog.surface);
                crate::systems::rendering::apply_selection_highlight(&mut mat);
            }
        }
        break;
    }
}

fn push_revert(game_state: &mut GameState, dialog: &EditDialogState) {
    if !dialog.is_new {
        use crate::resources::EditDialogState;
        game_state.pending_prim_revert = Some(EditDialogState {
            prim_id: dialog.prim_id,
            name: dialog.original_name.clone(),
            shape: dialog.original_shape.clone(),
            position: dialog.original_position,
            rotation: dialog.original_rotation,
            scale: dialog.original_scale,
            color: dialog.original_color,
            original_texture_id: dialog.original_texture_id.clone(),
            original_model: dialog.original_model.clone(),
            path_cut_begin: dialog.original_path_cut_begin,
            path_cut_end: dialog.original_path_cut_end,
            hollow: dialog.original_hollow,
            warp: dialog.original_warp,
            surface: dialog.original_surface,
            ..Default::default()
        });
    }
}

/// True when any twist/taper/top-shear/slice value differs from the prim's.
fn warp_differs(w: &crate::resources::PrimWarp, prim: &Prim) -> bool {
    (w.twist_begin - prim.twist_begin).abs() > 0.001
        || (w.twist_end - prim.twist_end).abs() > 0.001
        || (w.taper_x - prim.taper_x).abs() > 0.001
        || (w.taper_y - prim.taper_y).abs() > 0.001
        || (w.top_shear_x - prim.top_shear_x).abs() > 0.001
        || (w.top_shear_y - prim.top_shear_y).abs() > 0.001
        || (w.slice_begin - prim.slice_begin).abs() > 0.001
        || (w.slice_end - prim.slice_end).abs() > 0.001
}

/// Copy the dialog's warp parameters onto a prim component.
fn apply_warp(prim: &mut Prim, w: &crate::resources::PrimWarp) {
    prim.twist_begin = w.twist_begin;
    prim.twist_end = w.twist_end;
    prim.taper_x = w.taper_x;
    prim.taper_y = w.taper_y;
    prim.top_shear_x = w.top_shear_x;
    prim.top_shear_y = w.top_shear_y;
    prim.slice_begin = w.slice_begin;
    prim.slice_end = w.slice_end;
}

/// Send prim authoring changes to the server (ADR-029: online-only, geo-anchored)
/// and apply them locally for immediate feedback.
pub fn send_prim_mutations(
    mut commands: Commands,
    mut game_state: ResMut<GameState>,
    mut prim_query: Query<(
        Entity,
        &mut Prim,
        &mut Transform,
        Option<&MeshMaterial3d<StandardMaterial>>,
        Has<crate::components::OwnedMaterial>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    texture_cache: Res<PrimTextureCache>,
    online: Option<Res<OnlineSession>>,
    map_anchor: Res<crate::resources::MapAnchor>,
    mut edit_dialog: ResMut<EditDialogState>,
) {
    // The dialog's Physical box: sent the moment it is ticked, answered by the
    // server's `PrimUpsert` (or a refusal, which the next upsert corrects).
    if let Some(on) = edit_dialog.physical_request.take() {
        if let (Some(prim_id), Some(sess)) = (edit_dialog.prim_id, online.as_ref()) {
            sess.send(
                "prim physics",
                NetMessage::SetPrimPhysical {
                    request_id: 0,
                    prim_id,
                    physical: on,
                },
            );
        }
    }
    if let Some(dialog_state) = game_state.pending_prim_save.take() {
        if dialog_state.is_new {
            let pos = Vec3::new(
                dialog_state.position[0],
                dialog_state.position[1],
                dialog_state.position[2],
            );

            // ADR-029: prims are online-only and geo-anchored. Ask the server to
            // create the prim; its PrimUpsert echo spawns the entity locally with
            // the server-assigned id.
            let (Some(sess), Some(geo)) = (
                online.as_ref(),
                crate::systems::prim_geo::geo_from_world(pos, &map_anchor),
            ) else {
                tracing::warn!("cannot create prim: offline or world not anchored yet");
                return;
            };
            sess.send(
                "create prim",
                NetMessage::CreatePrim {
                    request_id: 0,
                    name: dialog_state.name.clone(),
                    position: pos,
                    rotation: Vec3::new(
                        dialog_state.rotation[0],
                        dialog_state.rotation[1],
                        dialog_state.rotation[2],
                    ),
                    scale: Vec3::new(
                        dialog_state.scale[0],
                        dialog_state.scale[1],
                        dialog_state.scale[2],
                    ),
                    color: dialog_state.color,
                    texture_id: dialog_state.texture_id.clone(),
                    shape: dialog_state.shape.clone(),
                    surface: dialog_state.surface,
                    geometry: dialog_state.geometry(),
                    geo,
                    // Nothing in the create dialog authors an emitter, so a prim
                    // made here starts without one.
                    particles: None,
                    model: dialog_state.model.clone(),
                },
            );
            tracing::info!(name = %dialog_state.name, "sent CreatePrim (online)");
            return;
        } else if let Some(prim_id) = dialog_state.prim_id {
            // ADR-029: send the authoritative, geo-anchored update to the server.
            // We still apply it locally below for immediate feedback; the PrimUpsert
            // echo is skipped for the selected prim, so there's no fight.
            let pos = Vec3::new(
                dialog_state.position[0],
                dialog_state.position[1],
                dialog_state.position[2],
            );
            if let (Some(sess), Some(geo)) = (
                online.as_ref(),
                crate::systems::prim_geo::geo_from_world(pos, &map_anchor),
            ) {
                sess.send(
                    "update prim",
                    NetMessage::UpdatePrim {
                        request_id: 0,
                        prim_id,
                        position: pos,
                        rotation: Vec3::new(
                            dialog_state.rotation[0],
                            dialog_state.rotation[1],
                            dialog_state.rotation[2],
                        ),
                        scale: Vec3::new(
                            dialog_state.scale[0],
                            dialog_state.scale[1],
                            dialog_state.scale[2],
                        ),
                        color: dialog_state.color,
                        texture_id: dialog_state.texture_id.clone(),
                        name: dialog_state.name.clone(),
                        surface: dialog_state.surface,
                        geometry: dialog_state.geometry(),
                        geo,
                        // The edit dialog has no emitter field, and `UpdatePrim`
                        // replaces the whole prim — so the current one is sent
                        // back, or saving a colour change would put out a fire a
                        // script had lit.
                        particles: prim_query
                            .iter()
                            .find(|(_, p, ..)| p.id == prim_id)
                            .and_then(|(_, p, ..)| p.particles),
                        model: dialog_state.model.clone(),
                    },
                );
            }
            for (entity, mut prim, mut transform, mat_handle_opt, owned) in prim_query.iter_mut() {
                if prim.id == prim_id {
                    let texture_changed = prim.texture_id != dialog_state.texture_id;
                    let geom_changed = prim.shape != PrimShape::from_str(&dialog_state.shape)
                        || (prim.path_cut_begin - dialog_state.path_cut_begin).abs() > 0.0001
                        || (prim.path_cut_end - dialog_state.path_cut_end).abs() > 0.0001
                        || (prim.hollow - dialog_state.hollow).abs() > 0.0001
                        || warp_differs(&dialog_state.warp, &prim);

                    prim.name = dialog_state.name.clone();
                    prim.shape = PrimShape::from_str(&dialog_state.shape);
                    prim.model = dialog_state.model.clone();
                    prim.color = Color::srgb(
                        dialog_state.color[0],
                        dialog_state.color[1],
                        dialog_state.color[2],
                    );
                    prim.texture_id = dialog_state.texture_id.clone();
                    prim.path_cut_begin = dialog_state.path_cut_begin;
                    prim.path_cut_end = dialog_state.path_cut_end;
                    prim.hollow = dialog_state.hollow;
                    prim.surface = dialog_state.surface;
                    apply_warp(&mut prim, &dialog_state.warp);
                    *transform = Transform::from_xyz(
                        dialog_state.position[0],
                        dialog_state.position[1],
                        dialog_state.position[2],
                    )
                    .with_rotation(Quat::from_euler(
                        EulerRot::XYZ,
                        dialog_state.rotation[0],
                        dialog_state.rotation[1],
                        dialog_state.rotation[2],
                    ))
                    .with_scale(Vec3::new(
                        dialog_state.scale[0],
                        dialog_state.scale[1],
                        dialog_state.scale[2],
                    ));
                    apply_material_edits(
                        &mut commands,
                        entity,
                        mat_handle_opt,
                        owned,
                        &mut materials,
                        &texture_cache,
                        dialog_state.texture_id.as_deref(),
                        texture_changed,
                        prim.color,
                        &prim.surface,
                    );
                    if geom_changed {
                        commands.entity(entity).insert(NeedsMeshRebuild);
                    }
                    commands.entity(entity).insert(Selected);
                    game_state.selected_prim_id = Some(prim_id);
                    tracing::info!(id = prim_id, "updated prim");
                    break;
                }
            }
        }
    }

    if let Some(revert) = game_state.pending_prim_revert.take() {
        if let Some(prim_id) = revert.prim_id {
            for (entity, mut prim, mut transform, mat_handle_opt, owned) in prim_query.iter_mut() {
                if prim.id == prim_id {
                    let texture_changed = prim.texture_id != revert.original_texture_id;
                    let geom_changed = prim.shape != PrimShape::from_str(&revert.shape)
                        || (prim.path_cut_begin - revert.path_cut_begin).abs() > 0.0001
                        || (prim.path_cut_end - revert.path_cut_end).abs() > 0.0001
                        || (prim.hollow - revert.hollow).abs() > 0.0001
                        || warp_differs(&revert.warp, &prim);

                    prim.name = revert.name.clone();
                    prim.shape = PrimShape::from_str(&revert.shape);
                    prim.color = Color::srgb(revert.color[0], revert.color[1], revert.color[2]);
                    prim.texture_id = revert.original_texture_id.clone();
                    // Reverting the model too: `attach_prim_models` watches the
                    // component, so putting the old hash back is the whole of
                    // undoing a model swap.
                    prim.model = revert.original_model.clone();
                    prim.path_cut_begin = revert.path_cut_begin;
                    prim.path_cut_end = revert.path_cut_end;
                    prim.hollow = revert.hollow;
                    prim.surface = revert.surface;
                    apply_warp(&mut prim, &revert.warp);
                    *transform = Transform::from_xyz(
                        revert.position[0],
                        revert.position[1],
                        revert.position[2],
                    )
                    .with_rotation(Quat::from_euler(
                        EulerRot::XYZ,
                        revert.rotation[0],
                        revert.rotation[1],
                        revert.rotation[2],
                    ))
                    .with_scale(Vec3::new(
                        revert.scale[0],
                        revert.scale[1],
                        revert.scale[2],
                    ));
                    apply_material_edits(
                        &mut commands,
                        entity,
                        mat_handle_opt,
                        owned,
                        &mut materials,
                        &texture_cache,
                        revert.original_texture_id.as_deref(),
                        texture_changed,
                        prim.color,
                        &prim.surface,
                    );
                    if geom_changed {
                        commands.entity(entity).insert(NeedsMeshRebuild);
                    }
                    commands.entity(entity).insert(Selected);
                    game_state.selected_prim_id = Some(prim_id);
                    break;
                }
            }
        }
    }

    for prim_id in std::mem::take(&mut game_state.prims_to_delete) {
        if let Some(sess) = online.as_ref() {
            sess.send(
                "delete prim",
                NetMessage::DeletePrim {
                    request_id: 0,
                    prim_id,
                },
            );
        }
        for (entity, prim, _, _, _) in prim_query.iter() {
            if prim.id == prim_id {
                commands.entity(entity).despawn();
                tracing::info!(id = prim_id, "deleted prim");
                break;
            }
        }
    }
}

/// Applies a prim's texture (optionally) and surface params (transparency / glow /
/// full-bright / UV transform) onto its material in a single copy-on-write out of
/// the shared batching pool. Doing both in one call matters: the CoW clone is
/// taken from the pooled handle, so two separate CoWs in the same frame would each
/// start from the pool and the second would discard the first's change.
fn apply_material_edits(
    commands: &mut Commands,
    entity: Entity,
    mat_handle_opt: Option<&MeshMaterial3d<StandardMaterial>>,
    owned: bool,
    materials: &mut Assets<StandardMaterial>,
    cache: &PrimTextureCache,
    texture_id: Option<&str>,
    apply_texture: bool,
    color: Color,
    surface: &cartopolis_core::PrimSurface,
) {
    let Some(mat_handle) = mat_handle_opt else {
        return;
    };
    let Some(mut mat) = crate::systems::rendering::owned_material(
        commands,
        materials,
        entity,
        &mat_handle.0,
        owned,
    ) else {
        return;
    };
    if apply_texture {
        mat.base_color_texture = texture_id
            .and_then(|id| cache.handles.get(id))
            .cloned()
            .map(|h| h.into());
    }
    crate::systems::rendering::apply_surface(&mut mat, color, surface);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const SHAPES: &[(&str, &str)] = &[
    ("box", "Box"),
    ("sphere", "Sphere"),
    ("cylinder", "Cylinder"),
    ("cone", "Cone"),
];

fn shape_display_name(key: &str) -> &str {
    SHAPES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, label)| *label)
        .unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::egui_manager::UnpaintedPasses as _;

    /// **A bottom-anchored `Area` taller than the sizing pass allows settles at
    /// a wrong height and stays there.** egui lays an `Area` out once against
    /// `Spacing::default_area_size` (600×**400**), remembers the content's size,
    /// and uses that as the `max_rect` from then on. Overflow is meant to grow
    /// it back, but only if the content reports its real height — and a
    /// `ScrollArea` shrinks to the room it is given while a justified top-down
    /// layout hands the widgets past the bottom whatever is left. Container and
    /// content then agree on the truncated height: a *stable* wrong answer, so
    /// it neither flickers nor recovers.
    ///
    /// The main menu was ~470 pt of entries in a 400 pt sizing pass, and the four
    /// below the fold — Sign in…, the status readout, Developer tools and Exit —
    /// were simply absent. It reads as a menu that is merely short, which is how
    /// it survived a UI sweep aimed at this very card.
    ///
    /// This reproduces the mechanism rather than calling the system (which needs
    /// a `World`), and asserts the fix: the sizing pass gets the whole band, so
    /// the card ends up as tall as its content.
    #[test]
    fn a_tall_bottom_anchored_card_is_not_truncated_by_the_sizing_pass() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 900.0));
        let rows = 24;
        let row = 24.0;
        let wanted = rows as f32 * row;
        assert!(
            wanted > 400.0,
            "the case only exists above the default sizing pass"
        );

        let run = |default_size: Option<egui::Vec2>| {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            let mut last = 0.0;
            // Several frames: the failure is what it *settles* on, not frame one.
            for _ in 0..8 {
                let raw = egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                };
                let _ = ctx.run_ui_unpainted(raw, |ui| {
                    let mut area = egui::Area::new(egui::Id::new("card"))
                        .order(egui::Order::Foreground)
                        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -60.0));
                    if let Some(size) = default_size {
                        area = area.default_size(size);
                    }
                    area.show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_width(MENU_WIDTH);
                            egui::ScrollArea::vertical()
                                .max_height(816.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.with_layout(
                                        egui::Layout::top_down_justified(egui::Align::Min),
                                        |ui| {
                                            for i in 0..rows {
                                                ui.add_sized(
                                                    egui::vec2(MENU_WIDTH, row),
                                                    egui::Button::new(format!("row {i}")),
                                                );
                                            }
                                        },
                                    );
                                });
                        });
                    });
                });
                last = ctx
                    .memory(|m| m.area_rect(egui::Id::new("card")))
                    .map_or(0.0, |r| r.height());
            }
            last
        };

        let starved = run(None);
        let fixed = run(Some(egui::vec2(MENU_WIDTH, 816.0)));
        assert!(
            starved < wanted * 0.9,
            "the bug should reproduce without a sizing hint: {starved} vs {wanted}"
        );
        assert!(
            fixed >= wanted,
            "with the band as the sizing pass the card must hold its content: \
             {fixed} vs {wanted}"
        );
    }

    /// The menu card is a size-to-content `Area`, so nothing clamps it: what it
    /// asks for is what it takes, off the edge of the screen included. This is
    /// the number that decides whether the app menu — the only route to
    /// Settings, sign-in and Exit — is on the screen at all. The card it
    /// replaced folded half its contents away below this width; a card that is
    /// the same on every viewport has to fit the smallest one instead.
    #[test]
    fn the_menu_card_fits_a_phone() {
        // 411 dp at `ui_scale` 1.5, i.e. what `quality::Tier::Phone` resolves an
        // ordinary Android handset to.
        let viewport = 274.0;
        // `Frame::popup`'s margins and the card's stroke, generously.
        let card = MENU_WIDTH + 24.0;
        assert!(
            card <= viewport,
            "menu card {card} does not fit {viewport} pt"
        );
    }

    /// The object editor asks for 320 pt, which is wider than a phone. A
    /// `Window` clamps its position to the screen but never its width, so this
    /// is the only thing keeping the Save/Cancel row reachable.
    #[test]
    fn the_object_editor_is_clamped_to_the_viewport() {
        assert_eq!(dialog_width(1280.0, 320.0), 320.0);
        assert_eq!(dialog_width(274.0, 320.0), 258.0);
        assert_eq!(dialog_width(274.0, 380.0), 258.0);
        // …and never negative, whatever the viewport claims to be.
        assert!(dialog_width(0.0, 320.0) > 0.0);
    }
}
