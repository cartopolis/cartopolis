//! The state behind your face's sheet, the Appearance panel, the map's ring
//! and the object editor, which the component library draws (`lib_main_menu`,
//! `lib_avatar`, `lib_action_ring`, `lib_object_editor`); and the prim edits
//! they lead to — the live preview, the saves and the deletes.

use crate::components::{NeedsMeshRebuild, Prim, PrimShape, Selected};
use crate::resources::OnlineSession;
use crate::resources::RightPanel;
use crate::resources::{ContextMenuState, EditDialogState, GameState, PrimTextureCache};
use crate::systems::icons;
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;
use bevy::pbr::StandardMaterial;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use cartopolis_core::msg;
use cartopolis_core::NetMessage;

/// Your face's sheet: whether it is showing, and where the face that toggles
/// it was drawn.
///
/// A resource because three systems share it: the search bar's face
/// (`dock`) and the Community page's (`lib_chats`) open it, and
/// [`drive_main_menu`] runs it. A tap on a face is a tap outside the sheet
/// when the sheet looks for one; `face` is the face's rect from the frame
/// before, which the sheet leaves to the face to answer.
#[derive(Resource, Default)]
pub struct MainMenu {
    pub open: bool,
    /// Where a face was drawn last frame, ui points. Taken by the sheet each
    /// frame and written again by whichever face draws.
    pub face: Option<Rect>,
    /// When Sign out was first tapped (`Time<Real>`'s clock): a second tap
    /// within [`SIGN_OUT_ARMED_S`] signs out ([`MainMenu::tap_sign_out`]).
    sign_out_at: Option<f64>,
    /// The sheet as the library draws it (`lib_main_menu`), while it is open.
    pub lib_view: Option<MenuView>,
    /// The library's taps, done by [`drive_main_menu`] next frame.
    pub lib_acts: Vec<SheetAct>,
}

/// How long a first tap on Sign out stays armed, s.
const SIGN_OUT_ARMED_S: f64 = 3.0;

impl MainMenu {
    /// Sign out is armed at `now`: the next tap signs out.
    #[must_use]
    pub fn sign_out_armed(&self, now: f64) -> bool {
        self.sign_out_at.is_some_and(|t| now - t < SIGN_OUT_ARMED_S)
    }

    /// A tap on Sign out, at `now`: the first arms it, a second while armed
    /// answers true (sign out), and a stray tap times out.
    pub fn tap_sign_out(&mut self, now: f64) -> bool {
        if self.sign_out_armed(now) {
            self.sign_out_at = None;
            true
        } else {
            self.sign_out_at = Some(now);
            false
        }
    }
}

/// Where a sheet over the map stands, in ui points: standing on the World |
/// Community switch (a phone, and the Share sheet), or hanging under your
/// face (a desktop).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SheetSpot {
    /// `left` from the left edge, `bottom` up from the bottom edge.
    Bottom {
        left: f32,
        bottom: f32,
        width: f32,
        max_height: f32,
    },
    /// `right` in from the right edge, `top` down from the top.
    Hang {
        right: f32,
        top: f32,
        width: f32,
        max_height: f32,
    },
}

/// Your face's sheet as the library draws it (`lib_main_menu`), decided by
/// [`drive_main_menu`].
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct MenuView {
    pub spot: SheetSpot,
    pub signed_in: bool,
    /// Your name for your face's disc, signed in.
    pub face: Option<String>,
    /// The orange dot on your face.
    pub waiting: bool,
    /// The heading and the line under it (`who_line`).
    pub title: String,
    pub sub: String,
    /// Reports waiting, for an account that may run something.
    pub admin: Option<usize>,
    pub error: Option<String>,
    /// Developer tools and Share diagnostics are offered.
    pub dev: bool,
    pub diagnostics: Option<String>,
    /// The first tap on Sign out has armed it.
    pub armed: bool,
}

/// The sheet's heading and the line under it: who you are, or that you are
/// not signed in.
fn who_line(signed_in: bool, me: Option<&str>, server: &str) -> (String, String) {
    match (signed_in, me) {
        (true, Some(me)) => (
            me.to_string(),
            if server.is_empty() {
                "Signed in".to_string()
            } else {
                cartopolis_core::address::person(me, server)
            },
        ),
        (true, None) => ("Signed in".to_string(), String::new()),
        _ => (
            "Not signed in".to_string(),
            "Meet people, chat and join communities".to_string(),
        ),
    }
}

/// Width of the sheet on a desktop, where it hangs under your face. Fixed
/// rather than sized to its content. On a phone it is the screen's width, as
/// a bottom sheet is.
const MENU_WIDTH: f32 = 320.0;

/// The sheet's width on a viewport this wide.
#[must_use]
fn sheet_width(viewport_w: f32, compact: bool) -> f32 {
    if compact {
        viewport_w - 2.0 * theme::OSD_MARGIN
    } else {
        MENU_WIDTH.min(viewport_w - 2.0 * theme::OSD_MARGIN)
    }
}

/// The sheet's account half (`systems::session`), bundled into one parameter
/// because [`drive_main_menu`] would otherwise sit at Bevy's 16-parameter
/// ceiling — the same reason `network::AuthWriteback` exists.
///
/// `session` is a `ResMut` because **Sign out happens here**, not on the
/// sign-in card: it is one entry with one outcome.
#[derive(bevy::ecs::system::SystemParam)]
pub struct SignInMenu<'w> {
    session: ResMut<'w, crate::systems::session::SessionState>,
    identity: Res<'w, crate::resources::AuthIdentity>,
    #[cfg(feature = "comms")]
    screen: ResMut<'w, crate::systems::login_screen::LoginScreen>,
    #[cfg(feature = "comms")]
    chats: ResMut<'w, crate::systems::chats::ChatsView>,
    #[cfg(feature = "comms")]
    contacts: Res<'w, crate::systems::contacts::Contacts>,
}

/// What a row of the library's sheet asked for (`MainMenu::lib_acts`).
#[derive(Clone, Copy, PartialEq)]
pub enum SheetAct {
    Panel(RightPanel),
    Profile,
    ShareLink,
    SignIn,
    DevTools,
    Diagnostics,
    /// A tap on Sign out: arms it, or signs out when armed
    /// ([`MainMenu::tap_sign_out`]).
    SignOut,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Exit,
    /// The ✕.
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    Close,
}

/// Your face's sheet: who you are, and everything that is not a place on the
/// map — your profile, your avatar, your saved places, Models, Settings,
/// Admin, About, the developer tools, Sign out and Exit
/// (`docs/mockups/redesign`, v2-account, A3). It replaced the ☰ menu, whose
/// entries and what each does it keeps; what changed is where it opens and
/// how it looks: rows of the theme's lists, 50 pt tall, on a sheet.
///
/// **A bottom sheet on a phone, a card under your face on a desktop.** On a
/// phone the face is at the top and the thumb at the bottom, and a sheet
/// rising from the bottom is the shape a phone gives "more about you"; on a
/// desktop a card hangs from the face that opened it.
///
/// **Who you are comes first**, written out — it was the hover text of a "Log
/// out" row once, which a phone never shows. Signing out is last, in red, and
/// arms before it acts (`docs/mockups/ux-review`).
///
/// This decides all of it — where the sheet stands, what it offers, whether
/// Sign out is armed, a tap outside it — publishes it in
/// [`MainMenu::lib_view`] and does the taps handed back in
/// [`MainMenu::lib_acts`].
#[allow(clippy::too_many_arguments)]
pub fn drive_main_menu(
    metrics: Res<UiMetrics>,
    time: Res<Time<Real>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut menu: ResMut<MainMenu>,
    mut dev_state: ResMut<crate::resources::DevPanelState>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    mut app_exit: MessageWriter<AppExit>,
    mut sign_in: SignInMenu,
    // Written here, read and cleared by `diagnostics::share_diagnostics`.
    mut diagnostics: ResMut<crate::resources::DiagnosticsRequest>,
    prefs: Res<crate::resources::AppPrefs>,
    // Whether the pointer is over one of the library's screens, for the
    // library's sheet's tap-outside test. As DOM a tap on one never reaches
    // the canvas, so only the Bevy backend has to say.
    #[cfg(feature = "ui-probe")] lib_pointer: Option<Res<mithril_bevy::UiPointer>>,
    #[cfg(not(feature = "ui-probe"))] ui_input: Res<crate::systems::ui_input::UiInput>,
) {
    let face = menu.face.take();
    let lib_acts = std::mem::take(&mut menu.lib_acts);
    if !menu.open {
        if menu.lib_view.is_some() {
            menu.lib_view = None;
        }
        return;
    }

    let compact = metrics.compact();
    let viewport = metrics.viewport_pts();
    let width = sheet_width(viewport.x, compact);
    #[cfg(feature = "comms")]
    let demo = sign_in.chats.demo;
    #[cfg(not(feature = "comms"))]
    let demo = false;
    let signed_in = sign_in.session.is_online() || demo;
    let me = sign_in
        .identity
        .username
        .clone()
        .filter(|n| !n.is_empty())
        .or_else(|| demo.then(|| "jeroen".to_string()));
    let server = match sign_in.identity.server_name.as_deref() {
        None if demo => "cartopolis.org".to_string(),
        other => other.unwrap_or_default().to_string(),
    };
    #[cfg(feature = "comms")]
    let admin = sign_in.chats.admin.filter(|_| signed_in);
    #[cfg(feature = "comms")]
    let waiting = signed_in && !sign_in.contacts.incoming.is_empty();
    // In the web build the page around the World tab is who you are: its
    // profile, its requests, its Admin and its sign-in (`comms`).
    #[cfg(not(feature = "comms"))]
    let (admin, waiting) = (None, false);
    let now = time.elapsed_secs_f64();
    let armed = menu.sign_out_armed(now);
    let mut close = false;

    let spot = if compact {
        // A card standing on the World | Community switch, not over it: the
        // switch is the way back, and the sheet has to fit above it with Sign
        // out in sight (2026-11-11; it was a bottom sheet over the switch,
        // and Sign out was under the fold).
        let bottom = crate::systems::dock::reserved_bottom() + 8.0;
        SheetSpot::Bottom {
            left: ((viewport.x - width) / 2.0).max(0.0),
            bottom,
            width,
            max_height: (viewport.y - bottom - theme::OSD_MARGIN).max(160.0),
        }
    } else {
        let top = face.map_or(crate::systems::dock::reserved_top(), |f| f.max.y + 8.0);
        let right = face.map_or(viewport.x - theme::OSD_MARGIN, |f| f.max.x + 2.0);
        SheetSpot::Hang {
            right: viewport.x - right,
            top,
            width,
            max_height: (viewport.y - top - 24.0).max(160.0),
        }
    };

    let (title, sub) = who_line(signed_in, me.as_deref(), &server);
    let view = MenuView {
        spot,
        signed_in,
        face: me.clone().filter(|_| signed_in),
        waiting,
        title,
        sub,
        admin,
        error: sign_in.session.error.clone(),
        dev: prefs.developer_menu,
        diagnostics: diagnostics.last.clone(),
        armed,
    };
    // The tap that opened it is not a tap outside it.
    let fresh = menu.lib_view.is_none();
    if menu.lib_view.as_ref() != Some(&view) {
        menu.lib_view = Some(view);
    }
    // One tap a frame.
    let act = lib_acts.last().copied();

    // Dismissed by a press that landed outside the sheet — on none of the
    // library's screens, the sheet being one — but not on a face, which
    // toggles it itself (see `MainMenu::face`). A finger is the emulated
    // mouse (`touch`). Escape and Back are `systems::dismiss`'s.
    #[cfg(feature = "ui-probe")]
    let over_lib = lib_pointer.is_some_and(|p| p.owns_pointer());
    #[cfg(not(feature = "ui-probe"))]
    let over_lib = ui_input.overlay_owns_pointer();
    let clicked_out = !fresh
        && !over_lib
        && mouse.is_some_and(|m| m.just_pressed(MouseButton::Left))
        && windows
            .iter()
            .next()
            .and_then(Window::cursor_position)
            .map(|p| metrics.window_to_ui(p))
            .is_some_and(|p| !face.is_some_and(|f| f.contains(p)));

    let mut log_out = false;
    match act {
        Some(SheetAct::Panel(panel)) => {
            right_panel.toggle(panel);
            close = true;
        }
        #[cfg(feature = "comms")]
        Some(SheetAct::Profile) => {
            // Your profile is the Community's You page.
            let chats = &mut sign_in.chats;
            chats.open = None;
            chats.sub = None;
            chats.you = true;
            *right_panel = RightPanel::Chat;
            close = true;
        }
        #[cfg(feature = "comms")]
        Some(SheetAct::ShareLink) => {
            let chats = &mut sign_in.chats;
            chats.open = None;
            chats.you = true;
            chats.sub = Some(crate::systems::contacts::Sub::Add);
            chats.sub_root = Some(crate::systems::contacts::Sub::Add);
            *right_panel = RightPanel::Chat;
            close = true;
        }
        #[cfg(feature = "comms")]
        Some(SheetAct::SignIn) => {
            sign_in.screen.open = true;
            close = true;
        }
        // The sheet offers none of them without `comms` (`lib_main_menu`).
        #[cfg(not(feature = "comms"))]
        Some(SheetAct::Profile | SheetAct::ShareLink | SheetAct::SignIn) => {}
        Some(SheetAct::DevTools) => {
            dev_state.open = !dev_state.open;
            close = true;
        }
        Some(SheetAct::Diagnostics) => diagnostics.pending = true,
        // Arms in place, as `theme::confirm_button` does: the first tap
        // asks, the second signs out, and a stray tap times out.
        Some(SheetAct::SignOut) => {
            if menu.tap_sign_out(now) {
                log_out = true;
                close = true;
            }
        }
        Some(SheetAct::Exit) => {
            app_exit.write(AppExit::Success);
            close = true;
        }
        Some(SheetAct::Close) => close = true,
        None => {}
    }
    #[cfg(target_arch = "wasm32")]
    let _ = &mut app_exit;
    if log_out {
        crate::systems::session::sign_out(&mut sign_in.session, &mut store);
        *right_panel = RightPanel::None;
    }
    // Armed, the label falls back by itself with no repaint asked for (egui's
    // sheet had to `request_repaint`): this runs every frame, `armed` above is
    // read off the clock each time, and the app never stops presenting — idle,
    // `frame_pacing::present_rate` halves the rate, floored at 30 fps — so
    // the changed view reaches the library within a frame of the 3 s.
    if close || clicked_out {
        menu.open = false;
    }
}

/// The share of the screen the Appearance sheet covers on a phone. It is also
/// where the camera puts its aim, in NDC: a sheet covering `s` leaves NDC
/// `2s - 1 ..= 1`, whose middle is `s`.
pub(crate) const SHEET_SHARE: f32 = 0.5;

/// The avatar designer (☰ ▸ Appearance): shapes the MakeHuman body with
/// sliders (`lib_avatar`). The avatar wears the draft while the panel is
/// open (`HumanDraft`); Apply commits it to the persisted store, which
/// `human_avatar::rebuild_on_look_change` watches to rebuild the model.
///
/// This seeds the draft and aims the camera; the library's designer edits
/// the draft, applies it and says what it wants of the camera through the
/// chooser (`lib_close_up`, `lib_turn`).
///
/// **The menu only offers it in a session** (`drive_main_menu`), because an
/// appearance is a thing other people look at. This closes it on the *transition*
/// out of one rather than refusing to show whenever there is no session, and the
/// difference is not pedantry: a scripted capture is always offline
/// (`--ui appearance` sets `RightPanel` directly), so a flat "online or nothing"
/// rule would mean the wardrobe is the one panel no README image can ever show.
/// What is actually wrong is a panel left standing after a sign-out, and that is
/// an edge.
#[allow(clippy::too_many_arguments)]
pub fn drive_avatar_chooser(
    metrics: Res<UiMetrics>,
    mut chooser: ResMut<crate::resources::AvatarChooserState>,
    store: Res<crate::systems::user_store::UserStore>,
    mut right_panel: ResMut<RightPanel>,
    session: Res<crate::systems::session::SessionState>,
    mut was_online: Local<bool>,
    // The draft the avatar is rebuilt from while the designer is open.
    mut human_draft: ResMut<crate::systems::human_avatar::HumanDraft>,
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
    // On a phone the designer is a sheet across the bottom half: a panel that
    // filled the screen would hide the avatar it is shaping.
    let sheet = metrics.compact();
    // While the designer is open the avatar wears the draft; closing it, saved
    // or not, puts it back in the saved look. Compared first, so an idle
    // panel does not mark the resource changed every frame.
    let showing = (right_panel.is(RightPanel::Appearance) && chooser.seeded)
        .then(|| chooser.draft.human.clone())
        .flatten();
    // Aim at the middle of the body while the designer is open; on the Face
    // tab, come close and aim at the face instead: its controls move things
    // millimetres across, unreadable from the follow distance. (1.0 m to the
    // head at 1.62 m: the whole head and the shoulders.)
    // On a phone the sheet covers the lower half, so the aim point is put in
    // the middle of the half it leaves, not the middle of the screen: aiming
    // at the face put it right on the sheet's edge, under it but for the hair.
    // (0.9 m at the default 6 m: 2.5 m of the top half, the body with room.)
    let close = showing.is_some() && chooser.lib_close_up;
    let aim = if close {
        Some(1.62)
    } else {
        showing.is_some().then_some(0.9)
    };
    if camera.aim_height != aim {
        camera.aim_height = aim;
    }
    let lift = (sheet && showing.is_some()).then_some(SHEET_SHARE);
    if camera.aim_screen != lift {
        camera.aim_screen = lift;
    }
    let reach = close.then_some(1.0);
    if camera.aim_reach != reach {
        camera.aim_reach = reach;
    }
    let turn = if showing.is_some() {
        chooser.lib_turn
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
/// click is seen this frame; because `egui_manager::suppress_egui_for_screenshot` drops the
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
    #[cfg(all(not(target_arch = "wasm32"), feature = "comms"))]
    if std::mem::take(&mut shot.camera_frame) {
        commands.spawn(Screenshot::primary_window()).observe(
            |captured: On<bevy::render::view::screenshot::ScreenshotCaptured>,
             mut camera: ResMut<crate::systems::camera_screen::CameraScreen>| {
                camera.frame_in =
                    crate::systems::camera_screen::recorder::Grab::from_image(&captured.image);
            },
        );
    }
    // The three below hand their pixels to the app's chats, posts and camera
    // (`comms`); the web build never asks for them.
    #[cfg(feature = "comms")]
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
    #[cfg(feature = "comms")]
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
    #[cfg(feature = "comms")]
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

/// Saving the point under the cursor as a place: the ring's Save. Bundled to
/// keep [`drive_context_menu`] under Bevy's 16-parameter cap.
/// `BookmarksState` is `Res` and not `ResMut` on purpose: `request_address`
/// resolves the name through an interior-mutability channel, so the ring does
/// not have to contend with the Places panel for the resource every frame it
/// happens to be open.
#[derive(bevy::ecs::system::SystemParam)]
pub struct SaveHere<'w> {
    pub store: ResMut<'w, crate::systems::user_store::UserStore>,
    pub bookmarks: Res<'w, crate::systems::bookmarks::BookmarksState>,
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
/// object's instead: touch it, edit it, script it, delete it, follow it. On a
/// place's name (`systems::place_names`, a click or a long press) it is the
/// place's, and short: go there, route there, or read it through a lens.
///
/// At 16 system parameters this is at Bevy's cap. The next thing that needs world
/// access here should be grouped into a `SystemParam` rather than added to the
/// list — `camera_controls` hit the same wall and that is what it did, and so
/// did the library's ring ([`SaveHere`] made its room).
///
/// The component library draws the ring (`lib_action_ring`,
/// `action_ring::MapRingLib`); this decides everything and acts on the
/// choice.
#[allow(clippy::too_many_arguments)]
pub fn drive_context_menu(
    mut commands: Commands,
    pointer: crate::systems::action_ring::RingPointer,
    mut context_menu: ResMut<ContextMenuState>,
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    prim_query: Query<(Entity, &Prim, &Transform)>,
    selected_query: Query<Entity, With<Selected>>,
    mut route: ResMut<crate::systems::navigation::RouteState>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    mut script_editor: ResMut<crate::systems::script_editor::ScriptEditorState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    mut save_here: SaveHere,
    camera_state: Res<crate::resources::CameraState>,
    camera: crate::grid::AnchoredCamera,
    mut attach: ResMut<crate::systems::camera_attach::CameraAttach>,
    mut ring_lib: Option<ResMut<crate::systems::action_ring::MapRingLib>>,
) {
    use crate::resources::RightPanel;
    use crate::systems::action_ring::{self, RingAction, RingOutcome};
    use crate::systems::navigation::Endpoint;

    if !context_menu.visible {
        if let Some(lib) = ring_lib.as_deref_mut() {
            lib.0.hide();
        }
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
    let place = context_menu.place.clone();

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
                hint: "Write JavaScript that runs on this object",
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
    } else if place.is_some() && lat_lng.is_some() {
        // A name on the map seen from a distance: the first wish is to go and
        // look, so Go there leads. Kept short on purpose (asked 2026-11-02):
        // saving, sharing and plans for a whole town wait for a reason to be here.
        actions.push(RingAction {
            key: "go_there",
            icon: icons::CAM_FLY,
            label: "Go there",
            hint: "Fly the camera to this place",
        });
        actions.push(RingAction {
            key: "route_to",
            icon: icons::ROUTE,
            label: "Route here",
            hint: "Set this place as the destination",
        });
        actions.push(RingAction {
            key: "lenses",
            icon: icons::POLL,
            label: "Lenses",
            hint: "Read this place through one of the lenses",
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
        // One Share, to a chat, a community or a link (`bridges`): it was a
        // link here and a separate "Post here" beside it.
        actions.push(RingAction {
            key: "share",
            icon: icons::SHARE,
            label: "Share",
            hint: "Send this spot to a chat or a community, or copy a link",
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
            // The same act with the street taken away: a white room around
            // the spot to build a group of objects in (`systems::design_room`).
            actions.push(RingAction {
                key: "design",
                icon: icons::PALETTE,
                label: "Design here",
                hint: "Design an object for this spot in a white room",
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
        if let Some(lib) = ring_lib.as_deref_mut() {
            lib.0.hide();
        }
        return;
    }

    let (title, detail) = match (&prim, &place, lat_lng) {
        (Some((_, name)), _, _) => (name.clone(), "Object".to_string()),
        (None, Some(p), _) => (p.name.clone(), p.detail.clone()),
        (None, None, Some((lat, lng))) => ("This spot".to_string(), format!("{lat:.5}, {lng:.5}")),
        (None, None, None) => (String::new(), String::new()),
    };
    // The library's driver inserts the bridge its first frame: the ring opens
    // the frame after.
    let Some(ring_lib) = ring_lib.as_deref_mut() else {
        return;
    };
    // A new id per opening, so the press that opened it is not read as a press
    // off the ring (`action_ring::run`).
    let at = pointer.window_to_ui(context_menu.screen_pos);
    let id = action_ring::RingId::new((
        "map_ring",
        context_menu.screen_pos.x.to_bits(),
        context_menu.screen_pos.y.to_bits(),
    ));
    let outcome = action_ring::run(
        &mut ring_lib.0,
        pointer.read(),
        id,
        at,
        &title,
        &detail,
        &actions,
    );

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
        ("plans_here", Some(at), _) => context_menu.plans_here = Some(at),
        ("go_there", Some((lat, lng)), _) => {
            if let Some(p) = &place {
                context_menu.go_there = Some((lat, lng, p.view_altitude_m, p.name.clone()));
            }
        }
        ("lenses", Some((lat, lng)), _) => {
            if let Some(p) = &place {
                context_menu.lenses_at = Some((lat, lng, p.name.clone()));
            }
        }
        ("save", Some((lat, lng)), _) => {
            // Altitude and pose come from the camera rather than from the hit
            // point, because a saved place is a remembered *view* as well as a
            // coordinate (`CameraPose::apply`).
            if let Some(altitude) = camera_altitude {
                let id = save_here.store.add_bookmark(
                    crate::systems::bookmarks::PLACEHOLDER_NAME.to_string(),
                    lat,
                    lng,
                    altitude,
                    crate::systems::geo_nav::CameraPose::capture(&camera_state),
                );
                save_here.bookmarks.request_address(id, lat, lng);
                *right_panel = RightPanel::Places;
            }
        }
        ("share", Some((lat, lng)), _) => {
            let name = place
                .as_ref()
                .map_or_else(|| "this spot".to_string(), |p| p.name.clone());
            context_menu.share_at = Some((lat, lng, name));
        }
        ("build", _, _) => {
            let hit = context_menu.hit_point;
            open_new_object(&mut edit_dialog, &mut game_state, [hit.x, hit.y, hit.z]);
        }
        ("design", Some(at), _) => context_menu.design_here = Some(at),
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
                    cartopolis_core::NetMessage::TouchPrim(msg::TouchPrim {
                        request_id: 0,
                        prim_id: *prim_id,
                    }),
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

/// Open the object editor on a new grey box standing at `ground` (anchored
/// world metres): the ring's Build, and the design room's Add a part.
pub fn open_new_object(
    edit_dialog: &mut EditDialogState,
    game_state: &mut GameState,
    ground: [f32; 3],
) {
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
    // Offset Y so the prim rests on the surface instead of being centred in it.
    edit_dialog.position = [ground[0], ground[1] + 0.5, ground[2]];
    edit_dialog.rotation = [0.0, 0.0, 0.0];
    edit_dialog.scale = [1.0, 1.0, 1.0];
    edit_dialog.visible = true;
    game_state.editing_prim_id = None;
}

/// Width of the object editor and its texture picker, clamped to the viewport.
///
/// The floor is deliberately below anything comfortable: on a viewport narrower
/// than the floor the dialog is cut off either way, and a width that goes
/// negative lays out nothing at all.
pub(crate) fn dialog_width(viewport_w: f32, preferred: f32) -> f32 {
    preferred.min(viewport_w - 16.0).max(200.0)
}

/// How much of a phone's screen height the object editor may cover. The rest is
/// the object itself, which is the thing being edited.
pub(crate) const SHEET_HEIGHT: f32 = 0.55;

/// What a button of the object editor or its texture picker asked for
/// (`lib_object_editor`).
#[derive(Clone, Debug, PartialEq)]
pub enum EditAct {
    /// The header's ✕: Cancel, and the picker goes too.
    Close,
    Save,
    Cancel,
    /// The second press of Delete: a deleted prim has no undo.
    Delete,
    /// Pick…: the texture picker opens, or closes again.
    TogglePicker,
    /// The texture comes off.
    ClearTexture,
    /// A tile of the picker: the texture goes on and the picker closes.
    PickTexture(String),
    /// The picker's ✕.
    ClosePicker,
    /// Repeats per metre's Apply: the per-face repeats from the object's size.
    ApplyPerMetre,
    ChooseVideo,
    StopVideo,
    /// The Physical box, ticked or cleared: sent at once, not on Save.
    Physical(bool),
}

/// Do `act` to the editor: the one place both editions' buttons go through.
pub fn apply_edit_act(act: EditAct, edit_dialog: &mut EditDialogState, game_state: &mut GameState) {
    match act {
        EditAct::Close => {
            push_revert(game_state, edit_dialog);
            edit_dialog.visible = false;
            edit_dialog.texture_picker_open = false;
            game_state.editing_prim_id = None;
        }
        EditAct::Save => {
            game_state.pending_prim_save = Some(edit_dialog.clone());
            edit_dialog.visible = false;
            game_state.editing_prim_id = None;
        }
        EditAct::Cancel => {
            push_revert(game_state, edit_dialog);
            edit_dialog.visible = false;
            game_state.editing_prim_id = None;
        }
        EditAct::Delete => {
            if let Some(id) = edit_dialog.prim_id {
                game_state.prims_to_delete.push(id);
            }
            edit_dialog.visible = false;
            game_state.editing_prim_id = None;
        }
        EditAct::TogglePicker => {
            edit_dialog.texture_picker_open = !edit_dialog.texture_picker_open;
        }
        EditAct::ClearTexture => edit_dialog.texture_id = None,
        EditAct::PickTexture(id) => {
            edit_dialog.texture_id = Some(id);
            edit_dialog.texture_picker_open = false;
        }
        EditAct::ClosePicker => edit_dialog.texture_picker_open = false,
        EditAct::ApplyPerMetre => {
            if edit_dialog.repeats_per_meter > 0.0 {
                let rpm = edit_dialog.repeats_per_meter;
                edit_dialog.surface.repeat_u = rpm * edit_dialog.scale[0];
                edit_dialog.surface.repeat_v = rpm * edit_dialog.scale[1];
            }
        }
        EditAct::ChooseVideo => {
            edit_dialog.video_action = Some(crate::resources::VideoAction::Choose);
        }
        EditAct::StopVideo => {
            edit_dialog.video_action = Some(crate::resources::VideoAction::Stop);
        }
        EditAct::Physical(on) => {
            edit_dialog.physical = on;
            edit_dialog.physical_request = Some(on);
        }
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
    room: Res<crate::systems::design_room::DesignRoom>,
) {
    // The dialog's Physical box: sent the moment it is ticked, answered by the
    // server's `PrimUpsert` (or a refusal, which the next upsert corrects).
    if let Some(on) = edit_dialog.physical_request.take() {
        if let (Some(prim_id), Some(sess)) = (edit_dialog.prim_id, online.as_ref()) {
            sess.send(
                "prim physics",
                NetMessage::SetPrimPhysical(msg::SetPrimPhysical {
                    request_id: 0,
                    prim_id,
                    physical: on,
                }),
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
                NetMessage::CreatePrim(msg::CreatePrim {
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
                    // Built in the design room: a draft only its maker sees until
                    // the room's Place puts it in the street (protocol 51).
                    draft: room.spot.is_some(),
                }),
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
                    NetMessage::UpdatePrim(msg::UpdatePrim {
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
                    }),
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
                NetMessage::DeletePrim(msg::DeletePrim {
                    request_id: 0,
                    prim_id,
                }),
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

pub(crate) const SHAPES: &[(&str, &str)] = &[
    ("box", "Box"),
    ("sphere", "Sphere"),
    ("cylinder", "Cylinder"),
    ("cone", "Cone"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Sign out arms on the first tap, acts on a second within the window,
    /// and a stray tap after the window only arms it again.
    #[test]
    fn sign_out_arms_before_it_acts() {
        let mut menu = MainMenu::default();
        assert!(!menu.tap_sign_out(10.0));
        assert!(menu.sign_out_armed(11.0));
        assert!(menu.tap_sign_out(11.0));
        assert!(!menu.sign_out_armed(11.5));
        assert!(!menu.tap_sign_out(20.0));
        assert!(!menu.tap_sign_out(20.0 + SIGN_OUT_ARMED_S + 1.0));
    }

    /// Your face's sheet is the only route to Settings, sign-in and Exit, so
    /// it must stay on screen. On a phone it is a
    /// bottom sheet exactly the screen's width (both documented phone
    /// viewports, 274 pt Android and ≈214 pt web); on a desktop a card under
    /// the face, inside the window's margins even at the narrowest
    /// non-compact width.
    #[test]
    fn the_sheet_fits_a_phone_and_a_window() {
        for viewport in [274.0_f32, 214.0] {
            let w = sheet_width(viewport, true);
            assert_eq!(w + 2.0 * theme::OSD_MARGIN, viewport);
            // Its rows have room for a word between the icon and the chevron:
            // 14 pt margins each side, a 58 pt icon column, a 36 pt end.
            assert!(w - 28.0 - 58.0 - 36.0 >= 60.0, "{w}");
        }
        for viewport in [640.0_f32, 1600.0] {
            let w = sheet_width(viewport, false);
            assert!(w + 2.0 * theme::OSD_MARGIN <= viewport, "{w} in {viewport}");
            assert_eq!(w, MENU_WIDTH);
        }
    }

    /// The object editor asks for 320 pt, which is wider than a phone; this
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
