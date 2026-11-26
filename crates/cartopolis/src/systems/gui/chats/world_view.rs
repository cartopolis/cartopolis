//! A chat's World view: one slim pill over the map (back, who, Chat |
//! World), and, while the dock's chat button has it open, the chat's
//! composer pill. This is its state: what the library draws
//! (`lib_chat_overlay`) is published here as [`WorldBar`], and its taps come
//! back as [`WorldTap`]s, done through the panel's own `Actions` and
//! `apply`.
//!
//! Everything else of the messenger is `chats`' state and the library's
//! screens (`lib_chats`).

use bevy::prelude::*;

use super::{
    apply, attach_entries, display_name, kind_of, pick_attach, share_for, top_offset, Actions,
    Attach, ChatKind, ChatsView,
};
use crate::resources::{OnlineSession, RightPanel};
use crate::systems::chat::ChatState;
use crate::systems::theme;
use crate::utils::clock::unix_ms;

/// Widest the World view's composer pill gets on a desktop window.
const WORLD_SHEET_MAX_WIDTH: f32 = 520.0;

/// A chat's World view as the library draws it, in points.
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) struct WorldBar {
    /// The chat's name, in the header.
    pub(crate) name: String,
    /// The header's top, down from the top of the screen.
    pub(crate) top: f32,
    /// The composer, while the dock's chat button has it open.
    pub(crate) composer: Option<WorldComposer>,
}

/// The World view's composer pill: "+", the field and send.
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) struct WorldComposer {
    /// Its left edge, from the screen's.
    pub(crate) left: f32,
    /// Its bottom edge, up from the screen's: above the keys or the dock.
    pub(crate) bottom: f32,
    pub(crate) width: f32,
    /// The "+" menu, while it is open.
    pub(crate) attach: Option<AttachMenu>,
    /// A private chat: the field's pin sends what the camera looks at.
    pub(crate) pin: bool,
    /// Something is typed: send can be pressed.
    pub(crate) ready: bool,
    /// Counts the times the field was asked to take the keyboard: a new one
    /// builds the field again, which takes it as it is built.
    pub(crate) claim: u32,
}

/// The "+" menu: round keys with a word under each, one icon for one
/// meaning (`docs/mockups/social-camera/`).
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) struct AttachMenu {
    /// A public chat: a note that places and locations go in private chats.
    pub(crate) public: bool,
    pub(crate) entries: Vec<Attach>,
    /// Location is open, showing how long to share for.
    pub(crate) share_open: bool,
    /// Already sharing with this chat: no durations.
    pub(crate) sharing: bool,
}

/// A tap on the World view, done by [`drive_chat_world`] the frame after.
#[derive(Clone, Copy, PartialEq, Debug)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) enum WorldTap {
    /// The header's back: to the chats.
    Back,
    /// The switch's Chat side: the thread, in the panel.
    Chat,
    /// "+": the menu open or shut.
    Plus,
    /// An entry of the "+" menu.
    Attach(Attach),
    /// Location's duration, in milliseconds.
    ShareFor(u64),
    /// The field's pin: send this view.
    ViewHere,
    Send,
}

/// A chat's World view: one slim pill over the map (back, who, Chat | World),
/// and nothing else of the chat's own. The chat's points are pinned by
/// `place_pins`.
///
/// **It reuses the map's chat rather than drawing a second one** (2026-10-18,
/// `docs/mockups/social-camera/`). Its last lines are the map's transcript,
/// bottom-left, fading, narrowed to this chat (`chat_overlay`); writing is the
/// dock's chat button, whose composer pill is this chat's here because a
/// private chat sends through the engine and not through a channel scope. The
/// dock, the joystick and the map buttons stay: the half-screen sheet this
/// replaced hid all of them, and stopped part-way down the screen.
///
/// Publishes [`ChatsView::lib_world`] every frame (`None` when the view is
/// not up) and does the taps the library left in
/// [`ChatsView::lib_world_taps`], and Back or Escape
/// ([`ChatsView::world_back`], the dismissal ladder's page rung).
#[allow(clippy::too_many_arguments)]
pub fn drive_chat_world(
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    mut overlay: ResMut<crate::systems::chat_overlay::ChatOverlay>,
    metrics: Res<crate::systems::ui_input::UiMetrics>,
    mut claims: Local<u32>,
) {
    let taps = std::mem::take(&mut view.lib_world_taps);
    let back = std::mem::take(&mut view.world_back);
    if view.lib_world.is_some() {
        view.lib_world = None;
    }
    let Some(key) = chat.world.clone() else {
        return;
    };
    if (online.is_none() && !view.demo) || driver.active || !right_panel.is(RightPanel::None) {
        return;
    }
    let Some(channel) = chat.channels.iter().find(|c| c.key == key) else {
        chat.world = None;
        return;
    };
    let kind = kind_of(&key);
    let name = display_name(&key, &channel.label);
    let n = channel.lines.len();
    view.mark_seen(&key, n);
    if view.open.as_deref() != Some(key.as_str()) {
        view.open_chat(&key);
    }

    let now = unix_ms();
    let mut act = Actions {
        back,
        ..Actions::default()
    };
    for tap in taps {
        match tap {
            WorldTap::Back => act.back = true,
            WorldTap::Chat => act.open = Some(key.clone()),
            WorldTap::Plus => view.attach = !view.attach,
            WorldTap::Attach(entry) => {
                if kind == ChatKind::Private {
                    pick_attach(entry, &mut view, &key, now, &mut act);
                }
            }
            WorldTap::ShareFor(ms) => share_for(&mut view, &key, ms, &mut act),
            WorldTap::ViewHere => {
                if ChatState::is_private(&key) {
                    act.view_to = Some(key.clone());
                }
            }
            WorldTap::Send => {
                if overlay.composing && !view.draft.trim().is_empty() {
                    act.send = Some(key.clone());
                }
            }
        }
    }
    // Sent, the pill goes, as the map's own does.
    if act.send.is_some() {
        overlay.close();
    }
    // "Chat" in the header goes back to the thread, which is the panel.
    if act.open.is_some() {
        chat.world = None;
        *right_panel = RightPanel::Chat;
    }
    if act.back {
        chat.world = None;
        view.open = None;
        *right_panel = RightPanel::Chat;
        act.back = false;
    }
    apply(
        act,
        &mut chat,
        &mut view,
        online.as_deref(),
        &mut right_panel,
        &mut nav,
        now,
    );
    // Left this frame: nothing to draw.
    if chat.world.as_deref() != Some(key.as_str()) || !right_panel.is(RightPanel::None) {
        return;
    }

    let composer = if overlay.composing {
        if overlay.take_claim() {
            view.claim_focus = true;
        }
        if std::mem::take(&mut view.claim_focus) {
            *claims = claims.wrapping_add(1);
        }
        let keys = crate::systems::chat_overlay::keyboard_reserve(&metrics, true);
        let (bottom, _) = crate::systems::chat_overlay::stack_bottoms(true, keys, 0.0);
        let viewport = metrics.viewport_pts();
        let width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(WORLD_SHEET_MAX_WIDTH);
        let attach = view.attach.then(|| AttachMenu {
            public: kind != ChatKind::Private,
            entries: attach_entries(&chat, &key),
            share_open: view.share_open,
            sharing: chat.sharing.iter().any(|s| s.channel == key),
        });
        Some(WorldComposer {
            left: (viewport.x - width) / 2.0,
            bottom,
            width,
            attach,
            pin: ChatState::is_private(&key),
            ready: !view.draft.trim().is_empty(),
            claim: *claims,
        })
    } else {
        None
    };
    view.lib_world = Some(WorldBar {
        name,
        top: top_offset(&chat),
        composer,
    });
}
