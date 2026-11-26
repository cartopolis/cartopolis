//! The messenger: a list of chats, and one chat at a time with two views of it
//! (docs/notes/chats-and-world.md; the design is `docs/mockups/chat-world/v5`,
//! and contacts are `v5-contacts` there and `systems::contacts` here).
//!
//! # Three screens, one state
//!
//!  * **World | Community**, the switch at the bottom of the screen
//!    (`dock::drive_dock`, since 2026-11-10; it was at the top of the map and
//!    only signed in). *World* is the map; *Community* is the list, which is
//!    the right-hand panel (the whole screen on a phone). It was called Chats
//!    until 2026-10-16, when it took the web page's word, since it holds
//!    communities and the public chats as well as conversations
//!    (`docs/mockups/home-and-profile`). Your face at the top right opens your
//!    sheet (`ui::drive_main_menu`), whose "Your profile" is the You page.
//!  * **A chat's Chat view**: its thread, as bubbles, with points drawn as
//!    numbered cards. Same panel, with the chat open.
//!  * **A chat's World view**: the map, with the chat's points as numbered pins
//!    (`place_pins`), its header at the top and its last lines and text field at
//!    the bottom. The panel is closed for this one: it *is* the map.
//!
//! Which of the three is showing is [`ChatsView::open`] plus the panel slot plus
//! [`ChatState::world`], and nothing else, so a chat is the same chat in both
//! views: same lines, same text field.
//!
//! # What stayed where it was
//!
//! Nearby chat is also the *Here* chat in the list, but the speech bubbles over
//! avatars and the floating composer on the map are untouched
//! (`chat_overlay`); they stand down only while a chat's World view has the
//! bottom of the screen. The chat state itself, the encryption and the network
//! are all unchanged — this module only holds what the screens show.
//!
//! # Who draws it
//!
//! The component library (`lib_chats` and the screens it holds) draws the
//! panel; [`drive_chats`] is the panel's state: what is read, the pictures
//! being picked, and what the library's dialogs and picture viewer are
//! handed. A chat's World view (its header and composer pills, drawn by
//! `lib_chat_overlay`) has its state in [`world_view`].

mod world_view;

pub use world_view::drive_chat_world;
#[cfg(feature = "ui-lib")]
pub(crate) use world_view::{AttachMenu, WorldComposer};
pub(crate) use world_view::{WorldBar, WorldTap};

use crate::utils::clock::unix_ms;
use bevy::prelude::*;
use cartopolis_core::meeting::Every;
use cartopolis_core::msg;
use cartopolis_core::{NetMessage, ReportReason};
use std::collections::HashMap;

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::chat::{ChatLine, ChatState, PersonAction, ReportDraft};
use crate::systems::user_store::SavedScreen;
use crate::systems::{icons, theme};
// The discs and buttons the world draws too (`systems::discs`), named here as
// they were before they moved out for the web build.
pub(crate) use crate::systems::discs::{kind_of, paint_avatar, tone_for, ChatKind};

#[derive(Resource, Default)]
pub struct ChatsView {
    /// The chat on screen, by channel key; `None` is the list.
    pub open: Option<String>,
    /// What is being typed into the open chat.
    pub draft: String,
    /// The list's "New chat" section is open.
    pub new_chat: bool,
    /// Which of its forms.
    pub new_kind: NewKind,
    /// The open chat's "+" menu is open.
    pub attach: bool,
    /// The "+" menu's Location is open, showing how long to share for.
    pub share_open: bool,
    /// "+ → Camera" asked for the camera, for this chat
    /// (`camera_screen::open_from_chat`).
    pub camera_for: Option<String>,
    /// "+ → Upload" asked for a picture from the device, for this chat. Taken
    /// where the capture is polled, so it works from the World view too.
    pub upload_for: Option<String>,
    /// How many of each chat's lines have been seen, for the unread counts.
    seen: HashMap<String, usize>,
    /// Put the caret in the open chat's field on the next frame it is drawn.
    claim_focus: bool,
    /// A scripted capture with no session behind it (`--ui chats`), the same
    /// switch `ChatOverlay::demo` is: it lets the screens draw offline.
    pub demo: bool,
    /// A contacts screen shown instead of the list (`systems::contacts`).
    pub sub: Option<crate::systems::contacts::Sub>,
    /// A picture being picked or taken, and the chat it goes to. One at a time:
    /// every platform arm puts a modal in front of the person.
    capture: Option<(String, crate::platform::photo::Capture)>,
    /// The picture open full size, if any.
    viewing: Option<crate::systems::chat_pictures::PictureRef>,
    /// Why the last picture could not be picked, until the next try.
    picture_error: Option<String>,
    /// The You page is showing instead of the list (your disc, top right).
    pub you: bool,
    /// What is typed into the list's search field: the list shows only the
    /// chats and communities whose name or last line holds it.
    pub search: String,
    /// The contacts screen the You page opened, so its back arrow returns to
    /// the You page rather than to the contacts list.
    pub sub_root: Option<crate::systems::contacts::Sub>,
    /// Settings was opened from the You page: its home gets a back arrow to
    /// there instead of a close button (`settings::render_settings_panel`).
    pub settings_from_you: bool,
    /// Sign out was pressed on the You page, for [`apply_sign_out`].
    pub sign_out: bool,
    /// How many reports wait, for the You page's Admin row; `None` for an
    /// account that may run nothing, which then has no Admin row at all.
    /// Written by `admin::drive_admin`.
    pub admin: Option<usize>,
    /// Show in world was pressed on this place (`bridges::drive_bridges`
    /// takes it, with the screen it was pressed on).
    pub show_in_world: Option<cartopolis_core::point::Point>,
    /// A meeting or a poll being written for a chat (`+ → Plan a meeting`,
    /// `+ → Ask a poll`), in that chat's composer: (chat key, the form).
    pub meeting_form: Option<(String, MeetingForm)>,
    /// The account whose deletion is to be confirmed this frame, for
    /// `lib_dialogs`, which takes it each frame: `None` on a frame
    /// [`drive_chats`] did not hand it over (the panel shut).
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub lib_confirm: Option<String>,
    /// The picture to show full size this frame, for `lib_picture_viewer`,
    /// which takes it each frame: `None` on a frame [`drive_chats`] did not
    /// hand it over (the panel shut).
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub lib_viewing: Option<crate::systems::chat_pictures::PictureRef>,
    /// A chat's World view this frame, for `lib_chat_overlay` to draw;
    /// `None` when it is not up. Written by [`drive_chat_world`].
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub(crate) lib_world: Option<WorldBar>,
    /// The World view's taps since [`drive_chat_world`] last ran, left by
    /// `lib_chat_overlay`.
    pub(crate) lib_world_taps: Vec<WorldTap>,
    /// Back or Escape for the World view: the dismissal ladder's page rung
    /// (`dismiss`), done by [`drive_chat_world`] as its back.
    pub(crate) world_back: bool,
}

impl ChatsView {
    /// The picture viewer's close.
    pub fn close_picture(&mut self) {
        self.viewing = None;
        self.lib_viewing = None;
    }
}

/// Which screen the Chats panel shows (`lib_chats` routes on it).
#[derive(Debug, Clone, PartialEq)]
pub enum ChatsScreen {
    /// A community screen in front of the list (`communities::screen`).
    Communities,
    /// A contacts screen (`contacts::screen`).
    Contacts(crate::systems::contacts::Sub),
    /// Your own page, from your disc.
    You,
    /// The calendar (`calendar::screen`).
    Calendar,
    /// The list of chats.
    List,
    /// One chat's Chat view, by channel key.
    Chat(String),
}

impl ChatsScreen {
    /// Whether the component library draws this screen when it has the
    /// panel: every one, since the contacts screens (`lib_contacts`) and the
    /// calendar (`lib_calendar`) moved over.
    #[must_use]
    pub fn on_library(&self) -> bool {
        match self {
            ChatsScreen::Communities
            | ChatsScreen::Contacts(_)
            | ChatsScreen::You
            | ChatsScreen::Calendar
            | ChatsScreen::List
            | ChatsScreen::Chat(_) => true,
        }
    }
}

/// The fields of `+ → Plan a meeting` and `+ → Ask a poll`, the web chat's two
/// dialogs (`crates/atlas/web`, `chat.tsx`) as forms above the composer.
#[derive(Debug, Clone, PartialEq)]
pub enum MeetingForm {
    Plan {
        title: String,
        start_ms: u64,
        minutes: u32,
        /// One item a line; a number at the end is its minutes.
        agenda: String,
        text: String,
        /// How often it comes back, and every how many: `None` for once.
        repeat: Option<(Every, u32)>,
        /// The start of the series' last date; `None` for no end.
        until_ms: Option<u64>,
    },
    Poll {
        question: String,
        /// One option a line.
        options: String,
        multiple: bool,
    },
}

impl MeetingForm {
    /// A meeting at the next whole hour, an hour long, as the web chat starts one.
    #[must_use]
    pub fn plan(now_ms: u64) -> Self {
        MeetingForm::Plan {
            title: String::new(),
            start_ms: next_hour_ms(now_ms),
            minutes: 60,
            agenda: String::new(),
            text: String::new(),
            repeat: None,
            until_ms: None,
        }
    }

    #[must_use]
    pub fn poll() -> Self {
        MeetingForm::Poll {
            question: String::new(),
            options: String::new(),
            multiple: false,
        }
    }

    /// What the form posts, or `None` while it is not yet complete: a plan
    /// needs a title, a poll a question and two options.
    #[must_use]
    pub fn object(&self) -> Option<cartopolis_core::meeting::MeetingObject> {
        use cartopolis_core::meeting::{
            new_id, AgendaItem, MeetingObject, Plan, Poll, Repeat, MAX_ITEMS,
        };
        match self {
            MeetingForm::Plan {
                title,
                start_ms,
                minutes,
                agenda,
                text,
                repeat,
                until_ms,
            } => {
                let title = title.trim();
                (!title.is_empty()).then(|| {
                    MeetingObject::Plan(Plan {
                        id: new_id(),
                        title: title.to_string(),
                        start_ms: *start_ms,
                        end_ms: Some(start_ms + u64::from(*minutes) * 60_000),
                        text: Some(text.trim().to_string()).filter(|t| !t.is_empty()),
                        agenda: agenda
                            .lines()
                            .filter_map(AgendaItem::from_line)
                            .take(MAX_ITEMS)
                            .collect(),
                        repeat: repeat.map(|(every, interval)| Repeat {
                            every,
                            interval,
                            until_ms: until_ms
                                .and_then(|u| snap_until(*start_ms, every, interval, u)),
                        }),
                        skip: Vec::new(),
                    })
                })
            }
            MeetingForm::Poll {
                question,
                options,
                multiple,
            } => {
                let question = question.trim();
                let options: Vec<String> = options
                    .lines()
                    .map(str::trim)
                    .filter(|o| !o.is_empty())
                    .map(str::to_string)
                    .take(MAX_ITEMS)
                    .collect();
                (!question.is_empty() && options.len() >= 2).then(|| {
                    MeetingObject::Poll(Poll {
                        id: new_id(),
                        question: question.to_string(),
                        options,
                        multiple: *multiple,
                    })
                })
            }
        }
    }
}

/// The next whole hour on this device's clock, which is not a whole UTC hour
/// where the offset has minutes in it.
pub(crate) fn next_hour_ms(now_ms: u64) -> u64 {
    const HOUR: i64 = 3_600_000;
    let off = i64::from(local_offset_s(now_ms).unwrap_or(0)) * 1000;
    let local = now_ms as i64 + off;
    ((local.div_euclid(HOUR) + 1) * HOUR - off).max(0) as u64
}

impl ChatsView {
    /// Open a chat in its Chat view.
    pub fn open_chat(&mut self, key: &str) {
        if self.open.as_deref() != Some(key) {
            self.draft.clear();
        }
        self.open = Some(key.to_string());
        self.attach = false;
        self.claim_focus = false;
    }

    /// The screen the panel shows, as both editions route it.
    #[must_use]
    pub fn screen(
        &self,
        communities: &crate::systems::communities::Communities,
        calendar: &crate::systems::calendar::Calendar,
    ) -> ChatsScreen {
        match (&self.sub, &self.open) {
            (None, None) if communities.screen.is_some() => ChatsScreen::Communities,
            (Some(sub), _) => ChatsScreen::Contacts(sub.clone()),
            (None, None) if self.you => ChatsScreen::You,
            (None, None) if calendar.open => ChatsScreen::Calendar,
            (None, None) => ChatsScreen::List,
            (None, Some(key)) => ChatsScreen::Chat(key.clone()),
        }
    }

    fn unread(&self, chat: &ChatState, key: &str, lines: &[ChatLine]) -> usize {
        let seen = self.seen.get(key).copied().unwrap_or(0).min(lines.len());
        lines[seen..].iter().filter(|l| !is_mine(chat, l)).count()
    }

    /// Unread lines in one chat, by its key.
    pub(crate) fn unread_of(&self, chat: &ChatState, key: &str) -> usize {
        chat.channels
            .iter()
            .find(|c| c.key == key)
            .map_or(0, |c| self.unread(chat, key, &c.lines))
    }

    /// The first `n` lines of a chat have been read: it is on screen
    /// somewhere other than the panel (the map's chat sheet).
    pub(crate) fn mark_seen(&mut self, key: &str, n: usize) {
        if self.seen.get(key) != Some(&n) {
            self.seen.insert(key.to_string(), n);
        }
    }

    /// Unread lines over every chat but *Here*, whose lines are the map's
    /// own bubbles: the count the World | Community switch wears.
    pub(crate) fn unread_total(&self, chat: &ChatState) -> usize {
        chat.channels
            .iter()
            .filter(|c| kind_of(&c.key) != ChatKind::Here)
            .map(|c| self.unread(chat, &c.key, &c.lines))
            .sum()
    }

    /// A picture is being picked or taken for this chat.
    pub(crate) fn capturing_for(&self, key: &str) -> bool {
        self.capture.as_ref().is_some_and(|(k, _)| k == key)
    }

    /// Why the last picture could not be picked, until the next try.
    pub(crate) fn picture_error(&self) -> Option<&str> {
        self.picture_error.as_deref()
    }
}

/// The line under a chat's name: who can read it.
#[must_use]
pub fn subtitle(kind: ChatKind) -> String {
    match kind {
        ChatKind::Private => format!("{} Private · end-to-end encrypted", icons::LOCK),
        ChatKind::Here => "Nearby · public, whoever is within earshot".into(),
        ChatKind::Everyone => "Everyone on this server · public".into(),
        ChatKind::Topic => format!("{} A topic · end-to-end encrypted", icons::LOCK),
        ChatKind::Community => "A community".into(),
    }
}

/// The name a chat is shown under, without the padlock its label carries (the
/// padlock is in the subtitle here, where it can say what it means).
#[must_use]
pub fn display_name(key: &str, label: &str) -> String {
    match kind_of(key) {
        ChatKind::Private => crate::systems::private_chat::peer_of_label(label).to_string(),
        ChatKind::Here => "Here".into(),
        ChatKind::Everyone => "Everyone".into(),
        ChatKind::Topic | ChatKind::Community => label.to_string(),
    }
}

fn is_mine(chat: &ChatState, line: &ChatLine) -> bool {
    chat.is_mine(line)
}

/// `18:04`, in this device's time zone.
#[must_use]
pub fn clock_time(at_ms: u64) -> String {
    jiff::Timestamp::from_millisecond(at_ms as i64)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// A row of the chat list.
pub struct Row {
    pub key: String,
    pub name: String,
    pub kind: ChatKind,
    pub preview: String,
    pub time: String,
    pub unread: usize,
    /// When its last line was said, for the newest-first order.
    pub at_ms: u64,
    /// The community this row is, if it is one.
    pub community: Option<uuid::Uuid>,
}

/// What the view waiting to be sent is called, while one waits: its place's
/// name, or its coordinates.
#[must_use]
pub fn sending_name(chat: &ChatState) -> Option<String> {
    chat.point_draft.as_ref().map(|draft| {
        draft
            .name
            .clone()
            .unwrap_or_else(|| format!("{:.4}, {:.4}", draft.latitude, draft.longitude))
    })
}

/// What an open chat needs to know when it is a topic of a community.
#[derive(Clone)]
pub(crate) struct TopicContext {
    /// "# general".
    pub(crate) name: String,
    /// "Garage 44 · 12 people".
    pub(crate) sub: String,
    pub(crate) community: uuid::Uuid,
    pub(crate) community_name: String,
    /// Whether this account moderates the community.
    pub(crate) moderates: bool,
    pub(crate) group: Vec<u8>,
}

impl TopicContext {
    pub(crate) fn of(
        communities: &crate::systems::communities::Communities,
        key: &str,
    ) -> Option<Self> {
        let (c, t) = communities.topic(key)?;
        let (name, sub, community) = crate::systems::communities::topic_header(communities, key)?;
        Some(Self {
            name,
            sub,
            community,
            community_name: c.settings.name.clone(),
            moderates: c
                .role
                .is_some_and(cartopolis_core::CommunityRole::moderates),
            group: t.group.clone(),
        })
    }
}

/// Everything the panel decided while it was drawing, acted on afterwards
/// because the drawing closure holds the state borrowed.
#[derive(Default)]
pub(crate) struct Actions {
    pub(crate) to_world: bool,
    pub(crate) close: bool,
    pub(crate) open: Option<String>,
    pub(crate) back: bool,
    pub(crate) world_of: Option<String>,
    pub(crate) send: Option<String>,
    pub(crate) send_point_to: Option<String>,
    pub(crate) cancel_point: bool,
    pub(crate) view_to: Option<String>,
    pub(crate) go_to: Option<cartopolis_core::point::Point>,
    pub(crate) start_private: Option<String>,
    pub(crate) create_group: bool,
    /// Work for a community, handed to `Communities::ops`.
    pub(crate) community_ops: Vec<crate::systems::communities::CommunityOp>,
    /// Open a community's screen.
    pub(crate) show_community: Option<uuid::Uuid>,
    /// A community picked in the list: the World, with only its posts on it.
    pub(crate) community_world: Option<uuid::Uuid>,
    /// Open the directory, or the form for starting a community.
    pub(crate) community_find: bool,
    pub(crate) community_start: bool,
    /// Report a line to its community's moderators: (who, line, community,
    /// topic, seq).
    pub(crate) report_to_mods: Option<(String, Option<String>, uuid::Uuid, Vec<u8>, u64)>,
    /// A server admin suspends (`false`) or deletes (`true`) an account.
    pub(crate) admin: Option<(String, bool)>,
    pub(crate) share: Option<(String, u64)>,
    pub(crate) stop_share: Option<String>,
    pub(crate) show_location: Option<(f64, f64, String)>,
    /// Plan a route to a point: `(lat, lng, name)`.
    pub(crate) route_to: Option<(f64, f64, String)>,
    /// Plan a route to a person sharing their location, which follows them:
    /// `(chat key, username, lat, lng)`.
    pub(crate) route_to_person: Option<(String, String, f64, f64)>,
    /// `+ → Meet up` in this chat.
    pub(crate) meetup: Option<String>,
    /// Move the draft meet-up's time, in minutes.
    pub(crate) meetup_shift: Option<i64>,
    pub(crate) meetup_send: bool,
    pub(crate) meetup_cancel: bool,
    /// "I'm going" to a meet-up: (chat key, the meet-up).
    pub(crate) meetup_going: Option<(String, cartopolis_core::point::Point)>,
    /// An answer to a plan or a vote in a poll, to seal and send: (chat key,
    /// what).
    pub(crate) meeting: Option<(String, cartopolis_core::meeting::MeetingObject)>,
    pub(crate) block: Option<String>,
    pub(crate) unblock: Option<String>,
    pub(crate) report: Option<(String, Option<String>)>,
    pub(crate) message: Option<String>,
    pub(crate) dismiss_notice: bool,
    pub(crate) call: Option<crate::systems::calls::CallRequest>,
    /// Open a contacts screen.
    pub(crate) sub: Option<crate::systems::contacts::Sub>,
    /// Open a picture full size.
    pub(crate) view_picture: Option<crate::systems::chat_pictures::PictureRef>,
    /// Open (`true`) or leave (`false`) the You page.
    pub(crate) you: Option<bool>,
    /// Your face at the top of the list was tapped: your sheet.
    pub(crate) face: bool,
    /// Put another panel in this one's place: Admin, Settings, Appearance.
    pub(crate) panel: Option<RightPanel>,
    /// The contacts screen the You page opened, so back returns there.
    pub(crate) sub_root: Option<crate::systems::contacts::Sub>,
    /// Settings, opened from the You page.
    pub(crate) settings_from_you: bool,
    pub(crate) sign_out: bool,
    /// Open the calendar.
    pub(crate) calendar: bool,
    /// Open the calendar on this row.
    pub(crate) calendar_pick: Option<crate::systems::calendar::Pick>,
}

/// Do what the calendar's pages asked for, through the same [`Actions`] the
/// chat cards use, so an answer from the calendar is the answer from the card.
pub(crate) fn calendar_out(
    out: crate::systems::calendar::Out,
    calendar: &mut crate::systems::calendar::Calendar,
    view: &mut ChatsView,
    act: &mut Actions,
) {
    if out.back {
        calendar.open = false;
    }
    if out.save {
        crate::systems::calendar::mark_dirty(calendar);
    }
    act.open = out.open_chat;
    act.meeting = out.meeting;
    act.meetup_going = out.meetup_going;
    act.route_to = out.route_to;
    act.go_to = out.go_to;
    if let Some(group) = out.join {
        act.call = Some(crate::systems::calls::CallRequest::Join(group));
    }
    if let Some((key, day_ms)) = out.plan_in {
        // The chat's own form, on the day the calendar was showing, at the
        // clock time it would have proposed for today.
        let today = MeetingForm::plan(unix_ms());
        let mut form = today.clone();
        if let (MeetingForm::Plan { start_ms, .. }, MeetingForm::Plan { start_ms: t, .. }) =
            (&mut form, &today)
        {
            let clock = t.saturating_sub(crate::systems::calendar::day_start_ms(
                crate::systems::calendar::local_day(*t),
            ));
            *start_ms = (day_ms + clock).max(*t);
        }
        view.meeting_form = Some((key.clone(), form));
        act.open = Some(key);
    }
    if let Some(key) = out.meetup_in {
        act.meetup = Some(key.clone());
        act.open = Some(key);
    }
}

/// What a private chat's header offers about the chat's call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CallButton {
    /// Not a private chat, or a build that cannot call.
    Hidden,
    /// Nobody is in a call: start one.
    Call,
    /// Somebody is: join them.
    Join,
    /// This device is in it.
    HangUp,
}

pub(crate) fn call_button(calls: &crate::systems::calls::Calls, key: &str) -> CallButton {
    let Some(group) = crate::systems::private_chat::group_of(key) else {
        return CallButton::Hidden;
    };
    if !crate::systems::calls::Calls::available() {
        CallButton::Hidden
    } else if calls.active_group() == Some(group.as_slice()) {
        CallButton::HangUp
    } else if calls.in_call(&group).is_empty() {
        CallButton::Call
    } else {
        CallButton::Join
    }
}

/// The Chats panel's state, every frame; the library draws the panel
/// (`lib_chats`), its dialogs (`lib_dialogs`) and its picture viewer
/// (`lib_picture_viewer`).
///
/// Whatever the panel shows: the communities' admin answers become the
/// chat notice, and a picture being picked (`+ → Upload`, from the panel or a
/// chat's World view) is started and kept arriving — the picker is in front
/// of the panel. While the panel is up (online, or seeded for a capture):
/// the open chat's lines count as read, the account-delete confirmation and
/// the full-size picture are handed to the library, and a community's
/// picture is picked. The report form needs nothing from here: `lib_dialogs`
/// shows it whenever `ChatState::report` is set and a session is there.
pub fn drive_chats(
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    online: Option<Res<OnlineSession>>,
    right_panel: Res<RightPanel>,
    mut communities: ResMut<crate::systems::communities::Communities>,
) {
    for said in communities.take_admin_results() {
        chat.notice = Some(crate::systems::chat::ChatNotice {
            text: said,
            error: false,
        });
    }
    // "+ → Upload", from the panel or a chat's World view.
    if let Some(channel) = view.upload_for.take() {
        view.picture_error = None;
        match crate::platform::photo::start_from(crate::platform::photo::Source::File) {
            Ok(capture) => {
                if let Some((_, mut old)) = view.capture.take() {
                    old.cancel();
                }
                view.capture = Some((channel, capture));
            }
            Err(why) => view.picture_error = Some(why),
        }
    }
    // A picture being picked keeps arriving whether or not the panel is open:
    // the picker is in front of it.
    poll_picture_capture(&mut view, &mut chat);
    if !right_panel.is(RightPanel::Chat) || (online.is_none() && !view.demo) {
        return;
    }
    // Reading a chat is seeing its lines.
    if let Some(key) = view.open.clone() {
        if let Some(c) = chat.channels.iter().find(|c| c.key == key) {
            let n = c.lines.len();
            view.seen.insert(key, n);
        }
    }
    view.lib_confirm = communities.confirm_delete.clone();
    pick_community_picture(&mut communities);
    view.lib_viewing = view.viewing.clone();
}

/// Who is signed in, as the panel names them: the username and the full
/// address (`jeroen@cartopolis.org`). A scripted capture has nobody signed
/// in, and draws a stand-in.
#[must_use]
pub(crate) fn who_am_i(identity: &crate::resources::AuthIdentity, demo: bool) -> (String, String) {
    let mut me = identity.username.clone().unwrap_or_default();
    if me.is_empty() && demo {
        me = "jeroen".into();
    }
    let server = match identity.server_name.as_deref() {
        None if demo => "cartopolis.org",
        other => other.unwrap_or_default(),
    };
    let full = cartopolis_core::address::person(&me, server);
    (me, full)
}

/// What a community screen asked for, into the panel's [`Actions`]: both
/// editions' community screens hand their `Out` here.
pub(crate) fn communities_out(
    out: crate::systems::communities::Out,
    communities: &mut crate::systems::communities::Communities,
    act: &mut Actions,
) {
    if let Some(key) = out.open {
        communities.screen = None;
        act.open = Some(key);
    }
    if let Some(who) = out.message {
        communities.screen = None;
        act.message = Some(who);
    }
    // A public post's place: open it as a point in a chat opens, with the
    // community's posts pinned (`community_posts`).
    if let Some(point) = out.go_to {
        act.go_to = Some(point);
    }
    // The ✕: what World does from the list. The page stays where it was, so
    // opening Community again comes back to it.
    act.close |= out.close;
}

/// Everything a frame of the panel asked for, done: the face's sheet, the
/// calendar, a call, the communities' work, and then [`apply`]. The library's
/// panel (`lib_chats`) ends its frame here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn settle(
    mut act: Actions,
    chat: &mut ChatState,
    view: &mut ChatsView,
    online: Option<&OnlineSession>,
    right_panel: &mut RightPanel,
    nav: &mut crate::systems::geo_nav::GeoNavState,
    calls: &mut crate::systems::calls::Calls,
    communities: &mut crate::systems::communities::Communities,
    calendar: &mut crate::systems::calendar::Calendar,
    menu: &mut crate::systems::ui::MainMenu,
    now: u64,
) {
    // Your face at the top of the list: the same sheet as the search bar's.
    if std::mem::take(&mut act.face) {
        menu.open = !menu.open;
    }
    if std::mem::take(&mut act.calendar) {
        calendar.show(now);
    }
    if let Some(pick) = act.calendar_pick.take() {
        calendar.show(now);
        calendar.picked = Some(pick);
        calendar.every = false;
        calendar.page = crate::systems::calendar::Page::Detail;
    }
    if let Some(request) = act.call.take() {
        calls.request = Some(request);
    }
    communities.ops.append(&mut act.community_ops);
    if let Some(id) = act.show_community.take() {
        view.open = None;
        communities.show(id);
    }
    if let Some(id) = act.community_world.take() {
        view.open = None;
        communities.posts.show_only(id);
        chat.world = None;
        *right_panel = RightPanel::None;
    }
    if std::mem::take(&mut act.community_find) {
        view.new_chat = false;
        communities.find();
    }
    if std::mem::take(&mut act.community_start) {
        view.new_chat = false;
        communities.start();
    }
    if let Some((who, line, id, group, seq)) = act.report_to_mods.take() {
        chat.report = Some(ReportDraft {
            username: who,
            message: line,
            reason: ReportReason::Spam,
            note: String::new(),
            asset: None,
            community: Some((id, group, seq)),
        });
    }
    match act.admin.take() {
        Some((who, false)) => communities.admin_act(who, false),
        Some((who, true)) => communities.confirm_delete = Some(who),
        None => {}
    }
    if let Some(picture) = act.view_picture.take() {
        view.viewing = Some(picture);
    }
    apply(act, chat, view, online, right_panel, nav, now);
}

/// A community's picture asked for (`Communities::pick_picture`): start the
/// platform's picker, and take what it gives. The picker has nothing to draw
/// — the platform's own sheet is in front.
fn pick_community_picture(communities: &mut crate::systems::communities::Communities) {
    if std::mem::take(&mut communities.pick_picture) {
        match crate::platform::photo::start_from(crate::platform::photo::Source::File) {
            Ok(capture) => communities.capture = Some(capture),
            Err(why) => communities.picture_note = Some(why),
        }
    }
    if let Some(capture) = communities.capture.as_mut() {
        use crate::platform::photo::Outcome;
        match capture.poll() {
            Outcome::Pending => {}
            Outcome::Ready(bytes) => {
                communities.capture = None;
                communities.upload_picture(bytes);
            }
            Outcome::Cancelled => {
                communities.capture = None;
                communities.picture_for_post = false;
            }
            Outcome::Failed(why) => {
                communities.capture = None;
                communities.picture_for_post = false;
                communities.picture_note = Some(why);
            }
        }
    }
}

/// The list as it stands: the two public tiles and the conversations,
/// filtered by the search field and newest first, as `lib_chats` draws it.
pub struct ListModel {
    /// Here and Everyone, the two public chats, as tiles.
    pub public: Vec<Row>,
    /// People, groups and communities, newest first.
    pub rows: Vec<Row>,
    /// A view waits to be sent (`ChatState::point_draft`): a tap on a
    /// private chat sends it there.
    pub sending: bool,
    /// The search field holds something: the list is its answer.
    pub searching: bool,
}

/// The chat list's rows, worked out from the chats, the requests and the
/// communities.
pub fn list_model(
    chat: &ChatState,
    view: &ChatsView,
    contacts: &crate::systems::contacts::Contacts,
    communities: &mut crate::systems::communities::Communities,
) -> ListModel {
    let sending = chat.point_draft.is_some();
    let needle = view.search.trim().to_lowercase();
    let matches = |name: &str, preview: &str| {
        needle.is_empty()
            || name.to_lowercase().contains(&needle)
            || preview.to_lowercase().contains(&needle)
    };
    let requests = contacts.request_channels(chat);

    let row_of = |c: &crate::systems::chat::ChatChannel| -> Row {
        let visible: Vec<&ChatLine> = c
            .lines
            .iter()
            .filter(|l| !chat.is_blocked(&l.sender))
            .collect();
        let last = visible.last();
        // A camera picture's pose is for the agent; a person reads its note.
        let shown = |l: &ChatLine| crate::systems::private_chat::readable(&l.body).into_owned();
        let kind = kind_of(&c.key);
        let preview = match last {
            None => match kind {
                ChatKind::Here => "Whoever is near your avatar".into(),
                ChatKind::Everyone => "Everyone on this server".into(),
                _ => "No messages yet".into(),
            },
            Some(l) if is_mine(chat, l) => format!("You: {}", shown(l)),
            Some(l) if kind == ChatKind::Private && !c.label.contains(&l.sender) => {
                format!("{}: {}", l.sender, shown(l))
            }
            Some(l) if kind != ChatKind::Private => format!("{}: {}", l.sender, shown(l)),
            Some(l) => shown(l),
        };
        let at = last.map_or(0, |l| l.at_ms);
        Row {
            key: c.key.clone(),
            name: display_name(&c.key, &c.label),
            kind,
            preview,
            time: if at > 0 {
                clock_time(at)
            } else {
                String::new()
            },
            unread: if kind == ChatKind::Here {
                0
            } else {
                view.unread(chat, &c.key, &c.lines)
            },
            at_ms: at,
            community: None,
        }
    };
    let public: Vec<Row> = [ChatKind::Here, ChatKind::Everyone]
        .iter()
        .filter_map(|k| chat.channels.iter().find(|c| kind_of(&c.key) == *k))
        .map(row_of)
        .filter(|r| matches(&r.name, &r.preview))
        .collect();
    // One list, newest first: people, groups and communities together. A
    // community is one row; a tap opens its only topic, or the community.
    let mut rows: Vec<Row> = chat
        .channels
        .iter()
        .filter(|c| {
            kind_of(&c.key) == ChatKind::Private
                && !requests.contains(&c.key)
                // Topics are inside their community's row.
                && !communities.is_topic(&c.key)
        })
        .map(row_of)
        .collect();
    let unread = |key: &str| {
        chat.channels
            .iter()
            .find(|c| c.key == key)
            .map_or(0, |c| view.unread(chat, key, &c.lines))
    };
    for c in crate::systems::communities::list_rows(communities, chat, &unread) {
        rows.push(Row {
            key: format!("community:{}", c.id),
            name: c.name,
            kind: ChatKind::Community,
            preview: c.preview,
            time: if c.at_ms > 0 {
                clock_time(c.at_ms)
            } else {
                String::new()
            },
            unread: c.unread,
            at_ms: c.at_ms,
            community: Some(c.id),
        });
    }
    rows.retain(|r| matches(&r.name, &r.preview));
    rows.sort_by(|a, b| b.at_ms.cmp(&a.at_ms));
    ListModel {
        public,
        rows,
        sending,
        searching: !needle.is_empty(),
    }
}

/// A tap on a row of the list, for both editions. A community is a place on
/// the map before it is a conversation (2026-10-18): picking one goes to the
/// World with only its posts there; its page and topics are a tap on the
/// chip that says so (`community_posts::drive_filter_chip`). With a view
/// waiting to be sent, a private chat is where it goes.
pub(crate) fn tap_row(
    key: &str,
    kind: ChatKind,
    community: Option<uuid::Uuid>,
    sending: bool,
    act: &mut Actions,
) {
    if let Some(id) = community {
        act.community_world = Some(id);
    } else if sending && kind == ChatKind::Private {
        act.send_point_to = Some(key.to_string());
    } else {
        act.open = Some(key.to_string());
    }
}

/// Your face at the top of the list wears an orange dot: a message request,
/// or a report waiting for an admin.
#[must_use]
pub fn face_waiting(contacts: &crate::systems::contacts::Contacts, view: &ChatsView) -> bool {
    !contacts.incoming.is_empty() || view.admin.is_some_and(|n| n > 0)
}

/// The message-requests row above the list, as words: "2 message requests",
/// "sanne · 1 more", and the first two people for their discs. `None` while
/// nobody waits.
#[must_use]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub fn requests_summary(
    contacts: &crate::systems::contacts::Contacts,
) -> Option<(String, String, Vec<String>)> {
    let n = contacts.incoming.len();
    let first = contacts.incoming.first()?;
    let title = if n == 1 {
        "1 message request".to_string()
    } else {
        format!("{n} message requests")
    };
    let more = if n > 1 {
        format!(" · {} more", n - 1)
    } else {
        String::new()
    };
    let people = contacts
        .incoming
        .iter()
        .take(2)
        .map(|r| r.username.clone())
        .collect();
    Some((title, format!("{}{more}", first.username), people))
}

/// Which of the "+" sheet's forms is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NewKind {
    /// The four choices.
    #[default]
    Choose,
    Person,
    Group,
}

/// The four things the "+" sheet offers, for both editions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NewChoice {
    Person,
    Group,
    FindCommunity,
    StartCommunity,
}

/// What the "+" sheet says under its choices.
pub const NEW_NOTE: &str = "All four are end-to-end encrypted.";

impl NewChoice {
    pub const ALL: [NewChoice; 4] = [
        NewChoice::Person,
        NewChoice::Group,
        NewChoice::FindCommunity,
        NewChoice::StartCommunity,
    ];

    #[must_use]
    pub fn icon(self) -> &'static str {
        match self {
            NewChoice::Person => icons::CHAT,
            NewChoice::Group => icons::GROUP,
            NewChoice::FindCommunity => icons::SEARCH,
            NewChoice::StartCommunity => icons::CAT_CITY,
        }
    }

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            NewChoice::Person => "Message a person",
            NewChoice::Group => "Start a group",
            NewChoice::FindCommunity => "Find a community",
            NewChoice::StartCommunity => "Start a community",
        }
    }

    #[must_use]
    pub fn line(self) -> &'static str {
        match self {
            NewChoice::Person => "From your contacts, or by name",
            NewChoice::Group => "A few people you pick",
            NewChoice::FindCommunity => "Clubs, teams, neighbourhoods",
            NewChoice::StartCommunity => "Several topics, one set of people",
        }
    }
}

/// A choice picked on the "+" sheet: a form, or a community screen.
pub(crate) fn pick_new(choice: NewChoice, kind: &mut NewKind, act: &mut Actions) {
    match choice {
        NewChoice::Person => *kind = NewKind::Person,
        NewChoice::Group => *kind = NewKind::Group,
        NewChoice::FindCommunity => act.community_find = true,
        NewChoice::StartCommunity => act.community_start = true,
    }
}

/// A person's name is typed on "Message a person".
#[must_use]
pub fn new_person_ready(chat: &ChatState) -> bool {
    !chat.private_input.trim().is_empty()
}

/// A group has a name and somebody in it.
#[must_use]
pub fn new_group_ready(chat: &ChatState) -> bool {
    !chat.group_name_input.trim().is_empty() && !chat.group_members_input.trim().is_empty()
}

/// What a tap on the You page opens, for both editions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum YouEntry {
    ShareLink,
    Avatar,
    Contacts,
    Requests,
    Privacy,
    Settings,
    Admin,
    SignOut,
}

/// A You page entry, tapped.
pub(crate) fn pick_you(entry: YouEntry, act: &mut Actions) {
    use crate::systems::contacts::Sub;
    let sub = |act: &mut Actions, sub: Sub| {
        act.sub = Some(sub.clone());
        act.sub_root = Some(sub);
    };
    match entry {
        YouEntry::ShareLink => sub(act, Sub::Add),
        YouEntry::Avatar => act.panel = Some(RightPanel::Appearance),
        YouEntry::Contacts => sub(act, Sub::Contacts),
        YouEntry::Requests => sub(act, Sub::Requests),
        YouEntry::Privacy => sub(act, Sub::Privacy),
        YouEntry::Settings => {
            act.panel = Some(RightPanel::Settings);
            act.settings_from_you = true;
        }
        YouEntry::Admin => act.panel = Some(RightPanel::Admin),
        YouEntry::SignOut => act.sign_out = true,
    }
}

/// An answer to a plan's card: on a series it answers the date the card is
/// about, otherwise the plan.
pub(crate) fn rsvp_answer(
    plan: &cartopolis_core::meeting::Plan,
    date: u64,
    rsvp: cartopolis_core::meeting::Rsvp,
) -> cartopolis_core::meeting::MeetingObject {
    use cartopolis_core::meeting::AnswerFor;
    cartopolis_core::meeting::MeetingObject::Answer {
        plan: plan.id.clone(),
        rsvp,
        date: if plan.repeat.is_some() {
            AnswerFor::Date(date)
        } else {
            AnswerFor::First
        },
    }
}

/// Its planner takes `date` out of a series.
pub(crate) fn skip_date(
    plan: &cartopolis_core::meeting::Plan,
    date: u64,
) -> cartopolis_core::meeting::MeetingObject {
    let mut p = plan.clone();
    p.skip.push(date);
    cartopolis_core::meeting::MeetingObject::Update(p)
}

/// Its planner ends a series with `date`.
pub(crate) fn stop_after(
    plan: &cartopolis_core::meeting::Plan,
    date: u64,
) -> cartopolis_core::meeting::MeetingObject {
    let mut p = plan.clone();
    if let Some(r) = p.repeat.as_mut() {
        r.until_ms = Some(date);
    }
    p.skip.retain(|d| *d <= date);
    cartopolis_core::meeting::MeetingObject::Update(p)
}

/// A tap on a poll's `option`, given what this person chose so far (`mine`):
/// toggles it on a choose-any poll, picks or clears it on a choose-one.
pub(crate) fn poll_vote(
    poll: &cartopolis_core::meeting::Poll,
    mine: &[String],
    option: &str,
) -> cartopolis_core::meeting::MeetingObject {
    let picked = mine.iter().any(|m| m == option);
    let choices = if poll.multiple {
        let mut c = mine.to_vec();
        if picked {
            c.retain(|x| x != option);
        } else {
            c.push(option.to_string());
        }
        c
    } else if picked {
        Vec::new()
    } else {
        vec![option.to_string()]
    };
    cartopolis_core::meeting::MeetingObject::Vote {
        poll: poll.id.clone(),
        choices,
    }
}

/// Who is going to a meet-up and how far each is from it, as the card lists
/// them: `(name, status)`.
///
/// The proposer counts as going. A person's distance comes from the location
/// they are sharing with this chat — which "I'm going" turns on before the start
/// — measured on this phone; somebody going who is not sharing (yet) reads as
/// "going", and nothing more.
pub(crate) fn meetup_people(
    chat: &ChatState,
    key: &str,
    proposal: &ChatLine,
    thread: &[ChatLine],
) -> Vec<(String, String)> {
    let Some(place) = proposal.point.as_ref() else {
        return Vec::new();
    };
    let mut names: Vec<(String, bool)> = vec![(proposal.sender.clone(), is_mine(chat, proposal))];
    for l in thread {
        let answers = l
            .point
            .as_ref()
            .is_some_and(|p| p.meetup.is_some_and(|m| m.going) && p.same_meetup(place));
        if answers && !names.iter().any(|(n, _)| *n == l.sender) {
            names.push((l.sender.clone(), is_mine(chat, l)));
        }
    }
    // An answer this phone sent but has not seen echoed back yet.
    if chat
        .meetups_going
        .iter()
        .any(|(k, p)| k == key && p.same_meetup(place))
        && !names.iter().any(|(_, mine)| *mine)
    {
        names.push(("You".into(), true));
    }
    names
        .into_iter()
        .map(|(name, mine)| {
            let status = if mine {
                "going".to_string()
            } else {
                chat.locations
                    .iter()
                    .find(|l| l.channel == key && l.from == name)
                    .map_or_else(
                        || "going".to_string(),
                        |l| {
                            let d = cartopolis_core::geo::plane_distance_m(
                                [l.latitude, l.longitude],
                                [place.latitude, place.longitude],
                            );
                            if d < 60.0 {
                                "there".to_string()
                            } else {
                                format!("{} away", crate::systems::routing::format_distance(d))
                            }
                        },
                    )
            };
            (if mine { "You".to_string() } else { name }, status)
        })
        .collect()
}

/// This device's UTC offset at `at_ms`, in seconds — the time zone the thread's
/// own clock times are printed in (`day_label`, `clock_time`).
pub(crate) fn local_offset_s(at_ms: u64) -> Option<i32> {
    let ts = jiff::Timestamp::from_millisecond(at_ms as i64).ok()?;
    Some(jiff::tz::TimeZone::system().to_offset(ts).seconds())
}

/// "Today", "Yesterday", or the date, in this device's time zone.
pub(crate) fn day_label(at_ms: u64, now_ms: u64) -> String {
    let tz = jiff::tz::TimeZone::system();
    let (Ok(at), Ok(now)) = (
        jiff::Timestamp::from_millisecond(at_ms as i64),
        jiff::Timestamp::from_millisecond(now_ms as i64),
    ) else {
        return String::new();
    };
    let day = at.to_zoned(tz.clone()).date();
    let today = now.to_zoned(tz).date();
    if day == today {
        "Today".into()
    } else if today.yesterday().is_ok_and(|y| y == day) {
        "Yesterday".into()
    } else {
        day.strftime("%a %d %b").to_string()
    }
}

/// A private chat is a direct one when its label is a person rather than a
/// group's name — which the chat itself does not record, so this asks whether
/// anybody but that person has spoken in it.
pub(crate) fn channel_is_direct(chat: &ChatState, key: &str) -> bool {
    chat.channels.iter().find(|c| c.key == key).is_none_or(|c| {
        let peer = crate::systems::private_chat::peer_of_label(&c.label);
        c.lines.iter().all(|l| {
            is_mine(chat, l) || l.sender == peer || l.sender_id == 0 && l.sender.is_empty()
        })
    })
}

/// What a choice in the menu behind a speaker's name asks for, for the line
/// `sender` said (`seq`, its place in a topic's log) in this chat
/// (`lib_chat_conversation`'s menu ends here).
pub(crate) fn person_act(
    a: PersonAction,
    sender: &str,
    seq: Option<u64>,
    topic: Option<&TopicContext>,
    act: &mut Actions,
) {
    match a {
        PersonAction::Message => act.message = Some(sender.to_string()),
        PersonAction::Block => act.block = Some(sender.to_string()),
        PersonAction::Report(msg) => act.report = Some((sender.to_string(), msg)),
        PersonAction::ReportToModerators(msg) => {
            if let (Some(t), Some(seq)) = (topic, seq) {
                act.report_to_mods =
                    Some((sender.to_string(), msg, t.community, t.group.clone(), seq));
            }
        }
        PersonAction::DeleteForEveryone => {
            if let (Some(t), Some(seq)) = (topic, seq) {
                act.community_ops
                    .push(crate::systems::communities::CommunityOp::ModerateDelete(
                        t.group.clone(),
                        seq,
                    ));
            }
        }
        PersonAction::RemoveFromTopic | PersonAction::RemoveFromCommunity => {
            if let Some(t) = topic {
                let whole = matches!(a, PersonAction::RemoveFromCommunity);
                act.community_ops
                    .push(crate::systems::communities::CommunityOp::Remove {
                        id: t.community,
                        who: sender.to_string(),
                        topic: (!whole).then(|| t.group.clone()),
                        block: false,
                    });
            }
        }
        PersonAction::Suspend => act.admin = Some((sender.to_string(), false)),
        PersonAction::DeleteAccount => act.admin = Some((sender.to_string(), true)),
    }
}

/// Fold in a picture the person picked or took, and queue it for the chat it
/// was asked for in.
fn poll_picture_capture(view: &mut ChatsView, chat: &mut ChatState) {
    use crate::platform::photo::Outcome;
    let Some((_, capture)) = view.capture.as_mut() else {
        return;
    };
    match capture.poll() {
        Outcome::Pending => {}
        Outcome::Ready(bytes) => {
            if let Some((channel, _)) = view.capture.take() {
                chat.private_pictures
                    .push(crate::systems::chat_pictures::OutgoingPicture {
                        channel,
                        bytes,
                        caption: String::new(),
                        rgba: None,
                    });
            }
        }
        Outcome::Cancelled => view.capture = None,
        Outcome::Failed(why) => {
            view.capture = None;
            view.picture_error = Some(why);
        }
    }
}

/// One of the "+" menu's entries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Attach {
    Camera,
    Upload,
    ThisView,
    MeetUp,
    Meeting,
    Poll,
    Location,
}

impl Attach {
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Attach::Camera => icons::CAMERA,
            Attach::Upload => icons::UPLOAD,
            Attach::ThisView => icons::PIN,
            Attach::MeetUp => icons::FLAG,
            Attach::Meeting => icons::EVENT,
            Attach::Poll => icons::POLL,
            Attach::Location => icons::HERE,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Attach::Camera => "Camera",
            Attach::Upload => "Upload",
            Attach::ThisView => "This view",
            Attach::MeetUp => "Meet up",
            Attach::Meeting => "Meeting",
            Attach::Poll => "Poll",
            Attach::Location => "Location",
        }
    }

    pub(crate) fn hint(self) -> &'static str {
        match self {
            Attach::Camera => "Take a picture for this chat",
            Attach::Upload => {
                "A picture from this device. Encrypted here: only this chat can open it."
            }
            Attach::ThisView => "Send where the camera is looking, as a place people can tap",
            Attach::MeetUp => {
                "Propose a place and a time. Halfway between you when they are sharing \
                 where they are; otherwise where the map is now."
            }
            Attach::Meeting => "A call in this chat at a time, with an agenda",
            Attach::Poll => "A question with options; everybody here sees who chose what",
            Attach::Location => "Share your real location with this chat for a while",
        }
    }
}

/// What the "+" menu of a private chat offers here: the camera and the
/// picker where this build has them, and Location unless it is shared.
pub(crate) fn attach_entries(chat: &ChatState, key: &str) -> Vec<Attach> {
    let sharing = chat.sharing.iter().any(|s| s.channel == key);
    let mut entries = Vec::new();
    if crate::systems::camera_screen::available() {
        entries.push(Attach::Camera);
    }
    if crate::platform::photo::offers(crate::platform::photo::Source::File) {
        entries.push(Attach::Upload);
    }
    entries.extend([
        Attach::ThisView,
        Attach::MeetUp,
        Attach::Meeting,
        Attach::Poll,
    ]);
    if !sharing {
        entries.push(Attach::Location);
    }
    entries
}

/// An entry of the "+" menu picked, in the chat `key`.
pub(crate) fn pick_attach(
    entry: Attach,
    view: &mut ChatsView,
    key: &str,
    now: u64,
    act: &mut Actions,
) {
    match entry {
        Attach::Camera => {
            view.camera_for = Some(key.to_string());
            view.attach = false;
        }
        Attach::Upload => {
            view.upload_for = Some(key.to_string());
            view.attach = false;
        }
        Attach::ThisView => act.view_to = Some(key.to_string()),
        Attach::MeetUp => act.meetup = Some(key.to_string()),
        Attach::Meeting => {
            view.meeting_form = Some((key.to_string(), MeetingForm::plan(now)));
            view.attach = false;
        }
        Attach::Poll => {
            view.meeting_form = Some((key.to_string(), MeetingForm::poll()));
            view.attach = false;
        }
        Attach::Location => view.share_open = !view.share_open,
    }
}

/// How long Location shares for: its words and milliseconds.
pub(crate) const SHARE_FOR: [(&str, u64); 3] = [
    ("15 min", 15 * 60_000),
    ("1 hour", 60 * 60_000),
    ("8 hours", 480 * 60_000),
];
/// Under the durations.
pub(crate) const SHARE_NOTE: &str = "Only this chat sees it, and it stops by itself.";

/// Share the real location with the chat `key` for `ms`.
pub(crate) fn share_for(view: &mut ChatsView, key: &str, ms: u64, act: &mut Actions) {
    act.share = Some((key.to_string(), ms));
    view.share_open = false;
}

/// The dates a plan starting at `start_ms` with this rule would have, for
/// the next two years: what Until steps through.
pub(crate) fn series_dates(start_ms: u64, every: Every, interval: u32) -> Vec<u64> {
    let series = cartopolis_core::meeting::Plan {
        start_ms,
        repeat: Some(cartopolis_core::meeting::Repeat {
            every,
            interval,
            until_ms: None,
        }),
        ..Default::default()
    };
    series.dates(
        start_ms,
        start_ms + 730 * 86_400_000,
        &crate::systems::calendar::offset_s,
    )
}

/// The series' last date on or before `until_ms`; `None` when there is none.
fn snap_until(start_ms: u64, every: Every, interval: u32, until_ms: u64) -> Option<u64> {
    series_dates(start_ms, every, interval)
        .into_iter()
        .take_while(|d| *d <= until_ms)
        .last()
}

/// The plan form's Repeats choices (`docs/mockups/calendar/v2-repeat`, I).
pub(crate) const REPEATS: [(&str, Option<(Every, u32)>); 4] = [
    ("Never", None),
    ("Every week", Some((Every::Week, 1))),
    ("Every 2 weeks", Some((Every::Week, 2))),
    ("Every month", Some((Every::Month, 1))),
];

/// A Repeats choice: a new rule drops the end the old one had.
pub(crate) fn pick_repeat(
    repeat: &mut Option<(Every, u32)>,
    until_ms: &mut Option<u64>,
    rule: Option<(Every, u32)>,
) {
    if *repeat != rule {
        *repeat = rule;
        *until_ms = None;
    }
}

/// Where Until is in the series, by its date (after the start moved): its
/// index and date; `None` for no end.
pub(crate) fn until_at(
    start_ms: u64,
    repeat: Option<(Every, u32)>,
    until_ms: Option<u64>,
) -> Option<(usize, u64)> {
    let (every, interval) = repeat?;
    let until = until_ms?;
    let dates = series_dates(start_ms, every, interval);
    let i = dates.iter().position(|d| *d >= until)?;
    Some((i, dates[i]))
}

/// How Until moves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UntilStep {
    NoEnd,
    /// An end starts four meetings in.
    SetEnd,
    /// One meeting earlier, never before the second.
    Earlier,
    Later,
}

/// Move Until one step, a meeting at a time.
pub(crate) fn step_until(
    start_ms: u64,
    repeat: Option<(Every, u32)>,
    until_ms: &mut Option<u64>,
    step: UntilStep,
) {
    let Some((every, interval)) = repeat else {
        return;
    };
    let dates = series_dates(start_ms, every, interval);
    let last = dates.len().saturating_sub(1);
    let at = until_at(start_ms, repeat, *until_ms).map(|(i, _)| i);
    *until_ms = match (step, at) {
        (UntilStep::NoEnd, _) => None,
        (UntilStep::SetEnd, _) => dates.get(3.min(last)).copied(),
        (UntilStep::Earlier, Some(i)) => dates.get(i.saturating_sub(1).max(1).min(last)).copied(),
        (UntilStep::Later, Some(i)) => dates.get((i + 1).min(last)).copied(),
        (UntilStep::Earlier | UntilStep::Later, None) => *until_ms,
    };
}

/// The plan form's start moved by `minutes`: never back before the hour now
/// running, since a plan for the past is not something to ask people to.
pub(crate) fn shift_start(start_ms: &mut u64, minutes: i64, now: u64) {
    let moved = start_ms.saturating_add_signed(minutes * 60_000);
    *start_ms = if minutes < 0 {
        moved.max(next_hour_ms(now).saturating_sub(3_600_000))
    } else {
        moved
    };
}

/// The plan form's lengths, in minutes.
pub(crate) const DURATIONS: [u32; 6] = [15, 30, 45, 60, 90, 120];

/// "15 min", "1 h", "1.5 h".
pub(crate) fn duration_label(m: u32) -> String {
    if m < 60 {
        format!("{m} min")
    } else if m % 60 == 0 {
        format!("{} h", m / 60)
    } else {
        format!("{} h", f64::from(m) / 60.0)
    }
}

#[allow(clippy::too_many_arguments)]
fn apply(
    act: Actions,
    chat: &mut ChatState,
    view: &mut ChatsView,
    online: Option<&OnlineSession>,
    right_panel: &mut RightPanel,
    nav: &mut crate::systems::geo_nav::GeoNavState,
    now: u64,
) {
    if act.dismiss_notice {
        chat.notice = None;
    }
    // A line that failed to send goes back in its chat's box, ahead of
    // anything typed since, so nothing typed is lost to a failed send.
    for (key, text) in std::mem::take(&mut chat.unsent) {
        if view.open.as_deref() == Some(key.as_str()) {
            view.draft = if view.draft.trim().is_empty() {
                text
            } else {
                format!("{text}\n{}", view.draft)
            };
            view.claim_focus = true;
        } else {
            // Its chat is not open, so there is no box to put it in: say it
            // with the text, so it can be copied and sent again.
            chat.notice = Some(crate::systems::chat::ChatNotice {
                text: format!("A private message was not sent: \u{201c}{text}\u{201d}"),
                error: true,
            });
        }
    }
    if let Some(sub) = act.sub {
        view.sub = Some(sub);
        view.new_chat = false;
    }
    if let Some(root) = act.sub_root {
        view.sub_root = Some(root);
    }
    if let Some(you) = act.you {
        view.you = you;
        view.new_chat = false;
    }
    if act.sign_out {
        view.sign_out = true;
    }
    if let Some(panel) = act.panel {
        view.settings_from_you = act.settings_from_you;
        view.new_chat = false;
        *right_panel = panel;
    }
    if act.to_world || act.close {
        *right_panel = RightPanel::None;
        chat.world = None;
        view.you = false;
        view.new_chat = false;
    }
    if let Some(key) = act.open {
        view.open_chat(&key);
        chat.focus_channel(&key);
    }
    if act.back {
        view.open = None;
        view.attach = false;
        chat.world = None;
    }
    if let Some(key) = act.world_of {
        chat.world = Some(key);
        *right_panel = RightPanel::None;
    }
    if let Some(key) = act.send {
        let body = view.draft.trim().to_string();
        send_line(chat, online, view.demo, &key, body);
        view.draft.clear();
        view.claim_focus = true;
    }
    if let Some(key) = act.send_point_to {
        if let Some(mut point) = chat.point_draft.take() {
            let text = chat.point_text.trim().to_string();
            point.text = (!text.is_empty()).then_some(text);
            chat.private_points.push((key.clone(), point));
            chat.point_text.clear();
            view.open_chat(&key);
            chat.focus_channel(&key);
        }
    }
    if act.cancel_point {
        chat.point_draft = None;
        chat.point_text.clear();
    }
    if let Some(key) = act.view_to {
        chat.view_requests.push(key);
        view.attach = false;
    }
    // Meet-ups (`systems::meetup`): ask for a place, adjust the time, send.
    if let Some(key) = act.meetup {
        chat.meetup_requests.push(key);
        view.attach = false;
    }
    if let Some(minutes) = act.meetup_shift {
        let floor = crate::systems::meetup::default_start_ms(unix_ms()).saturating_sub(30 * 60_000);
        if let Some(m) = chat
            .meetup_draft
            .as_mut()
            .and_then(|(_, p)| p.meetup.as_mut())
        {
            // Never into the past: a meet-up that has already started is not
            // something to propose.
            m.start_ms = m
                .start_ms
                .saturating_add_signed(minutes * 60_000)
                .max(floor);
        }
    }
    if act.meetup_cancel {
        chat.meetup_draft = None;
    }
    if act.meetup_send {
        if let Some((key, point)) = chat.meetup_draft.take() {
            // The proposer is going to their own meet-up: the share and the
            // arrival apply to them as to everyone who answers.
            chat.meetups_going.push((key.clone(), point.clone()));
            chat.private_points.push((key, point));
        }
    }
    if let Some(said) = act.meeting {
        chat.private_meetings.push(said);
    }
    if let Some((key, place)) = act.meetup_going {
        let mut answer = place.clone();
        answer.text = Some("I'm going".into());
        answer.view = None;
        if let Some(m) = answer.meetup.as_mut() {
            m.going = true;
        }
        chat.private_points.push((key.clone(), answer));
        chat.meetups_going.push((key, place.clone()));
        // And the way there, which is what somebody going needs next.
        nav.route_to(
            place.name.clone().unwrap_or_else(|| "Meet-up".to_string()),
            place.latitude,
            place.longitude,
        );
    }
    if let Some(point) = act.go_to {
        // Show in world (`bridges`): the map flies there, with the place's
        // card and a way back to this screen.
        view.show_in_world = Some(point);
    }
    if let Some(name) = act.start_private.or(act.message) {
        chat.private_start.push(name);
        chat.private_input.clear();
        view.new_chat = false;
    }
    if act.create_group {
        let name = chat.group_name_input.trim().to_string();
        let members: Vec<String> = chat
            .group_members_input
            .split(',')
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
        chat.group_start.push((name, members));
        chat.group_name_input.clear();
        chat.group_members_input.clear();
        view.new_chat = false;
    }
    if let Some((key, duration)) = act.share {
        chat.start_sharing(&key, duration, now);
        view.attach = false;
    }
    if let Some(key) = act.stop_share {
        chat.stop_sharing(&key);
    }
    if let Some((lat, lng, who)) = act.show_location {
        view.show_in_world = Some(cartopolis_core::point::Point {
            latitude: lat,
            longitude: lng,
            name: Some(who),
            text: None,
            view: None,
            meetup: None,
        });
    }
    // Route: the planner opens on the destination (`geo_nav::RouteRequest`),
    // which replaces this panel in the right-hand slot.
    if let Some((lat, lng, name)) = act.route_to {
        nav.route_to(name, lat, lng);
    }
    if let Some((key, who, lat, lng)) = act.route_to_person {
        nav.route_to_person(&key, &who, lat, lng);
    }
    if let Some(online) = online {
        for (name, on) in [(act.block, true), (act.unblock, false)] {
            let Some(username) = name else { continue };
            online.send(
                "set blocked",
                NetMessage::SetBlocked(msg::SetBlocked {
                    request_id: 0,
                    username,
                    blocked: on,
                }),
            );
        }
    }
    if let Some((username, message)) = act.report {
        chat.report = Some(ReportDraft {
            username,
            message,
            reason: ReportReason::Harassment,
            note: String::new(),
            asset: None,
            community: None,
        });
    }
}

/// Send one line to a chat by the path its kind needs. A private line goes to
/// `private_chat` to be sealed and must never reach `ChatSend`, where the
/// server would read and store it.
fn send_line(
    chat: &mut ChatState,
    online: Option<&OnlineSession>,
    demo: bool,
    key: &str,
    body: String,
) {
    if body.is_empty() {
        return;
    }
    if ChatState::is_private(key) {
        if demo && online.is_none() {
            let me = chat.my_id.unwrap_or(1);
            chat.receive(key, me, "You".into(), body, None);
        } else {
            chat.private_outbox.push((key.to_string(), body));
        }
        return;
    }
    match online {
        Some(online) => {
            online.send(
                "chat send",
                NetMessage::ChatSend(msg::ChatSend {
                    request_id: 0,
                    scope: crate::systems::chat::scope_for(key, false),
                    body,
                }),
            );
        }
        None if demo => {
            let me = chat.my_id.unwrap_or(1);
            chat.receive(key, me, "You".into(), body, None);
        }
        None => {}
    }
}

/// How far down the top of the screen this module's overlays start: below the
/// "you are sharing your location" notice while one is up, which
/// `place_pins::sharing_chip` draws at the top centre and which must never be
/// covered.
pub(crate) fn top_offset(chat: &ChatState) -> f32 {
    if chat.sharing.is_empty() {
        theme::OSD_MARGIN
    } else {
        theme::OSD_MARGIN + 44.0
    }
}

/// What [`remember_screen`] would save now: the Chats tab and its open chat, or
/// a chat's World view (`chats: false` with a chat open), or plain World.
fn screen_now(chat: &ChatState, view: &ChatsView, panel: RightPanel) -> SavedScreen {
    if panel.is(RightPanel::Chat) {
        SavedScreen {
            chats: true,
            open: view.open.clone(),
        }
    } else {
        SavedScreen {
            chats: false,
            open: chat.world.clone(),
        }
    }
}

/// Keep the screen in `user.json` as it changes, so Android stopping the app in
/// the background does not also forget where the person was (`SavedScreen`).
///
/// Only while signed in and not mid-restore: the chats exist only then, and a
/// session still coming up (or a chat still arriving) would otherwise overwrite
/// the screen about to be put back with World.
pub fn remember_screen(
    chat: Res<ChatState>,
    view: Res<ChatsView>,
    panel: Res<RightPanel>,
    online: Option<Res<OnlineSession>>,
    restored: Res<ScreenRestore>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
) {
    if online.is_none() || view.demo || !restored.done || restored.pending.is_some() {
        return;
    }
    let now = screen_now(&chat, &view, *panel);
    if store.data.screen.as_ref() != Some(&now) {
        store.data.screen = Some(now);
        store.dirty = true;
    }
}

/// How far [`restore_screen`] has got with this session.
#[derive(Resource, Default)]
pub struct ScreenRestore {
    /// The saved screen has been put back for the session that is up now.
    pub done: bool,
    /// A chat to open once it exists: private chats arrive a moment after the
    /// session does, and a chat's World view whose chat is missing closes itself.
    pending: Option<(SavedScreen, web_time::Instant)>,
}

/// How long a remembered chat is waited for before the list is left showing.
const RESTORE_WAIT_SECS: f32 = 20.0;

/// Put the remembered screen back whenever a session comes up — at a cold start,
/// and after a resume, where the socket died while the phone was suspended and
/// the private chats are re-synced from nothing.
///
/// A call ringing is drawn over all of it (`call_screen`), so it needs no rule
/// here; anything else that already chose a panel (a place link, the sign-in
/// card's follow-up) wins over a memory.
pub fn restore_screen(
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    mut panel: ResMut<RightPanel>,
    online: Option<Res<OnlineSession>>,
    store: Res<crate::systems::user_store::UserStore>,
    mut restore: ResMut<ScreenRestore>,
) {
    if view.demo {
        return;
    }
    if online.is_none() {
        restore.done = false;
        restore.pending = None;
        return;
    }
    if !restore.done {
        restore.done = true;
        let Some(saved) = store.data.screen.clone() else {
            return;
        };
        let free = panel.is(RightPanel::None) || panel.is(RightPanel::Chat);
        if !free {
            return;
        }
        if saved.chats {
            *panel = RightPanel::Chat;
        }
        if saved.open.is_some() {
            restore.pending = Some((saved, web_time::Instant::now()));
        }
    }
    let Some((saved, since)) = restore.pending.clone() else {
        return;
    };
    let Some(key) = saved.open.as_deref() else {
        restore.pending = None;
        return;
    };
    // The person moved on while the chat was still arriving.
    let still_there = if saved.chats {
        panel.is(RightPanel::Chat) && view.open.as_deref().is_none_or(|k| k == key)
    } else {
        panel.is(RightPanel::None) && chat.world.as_deref().is_none_or(|k| k == key)
    };
    if !still_there || since.elapsed().as_secs_f32() > RESTORE_WAIT_SECS {
        restore.pending = None;
        return;
    }
    if chat.channels.iter().any(|c| c.key == key) {
        if saved.chats {
            view.open_chat(key);
        } else {
            chat.world = Some(key.to_string());
        }
        restore.pending = None;
    }
}

/// Sign out from the You page: the same path as your face's sheet's Sign out.
pub fn apply_sign_out(
    mut view: ResMut<ChatsView>,
    mut session: ResMut<crate::systems::session::SessionState>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
    mut right_panel: ResMut<RightPanel>,
) {
    if !std::mem::take(&mut view.sign_out) {
        return;
    }
    view.you = false;
    view.sub = None;
    view.sub_root = None;
    *right_panel = RightPanel::None;
    crate::systems::session::sign_out(&mut session, &mut store);
}

/// Seed the messenger for a scripted capture: three private chats, one with two
/// points in it, and the nearby chat. `screen` is `list`, `chat` or `world`.
///
/// For the reason `chat_overlay::seed_demo_chat` exists: a scripted run has no
/// server, and a screen nobody can photograph is a screen nobody reviews.
pub fn seed_demo_chats(chat: &mut ChatState, view: &mut ChatsView, screen: &str) -> RightPanel {
    const ME: u64 = 1;
    chat.my_id = Some(ME);
    let sanne = "dm:5a4e";
    if !chat.channels.iter().any(|c| c.key == sanne) {
        // Minutes before now, so the thread's day chip reads "Today".
        let start = unix_ms().saturating_sub(10 * 60_000);
        let at = move |min: u64| start + min * 60_000;
        chat.open_channel(sanne, &crate::systems::private_chat::private_label("Sanne"));
        let push = |chat: &mut ChatState,
                    key: &str,
                    id: u64,
                    who: &str,
                    body: &str,
                    point: Option<cartopolis_core::point::Point>,
                    min: u64| {
            chat.receive_point(key, id, who.into(), body.into(), None, point);
            if let Some(c) = chat.channels.iter_mut().find(|c| c.key == key) {
                if let Some(l) = c.lines.last_mut() {
                    l.at_ms = at(min);
                }
            }
        };
        let point = |lat: f64, lng: f64, name: &str, text: &str| cartopolis_core::point::Point {
            latitude: lat,
            longitude: lng,
            name: Some(name.into()),
            text: Some(text.into()),
            view: Some(cartopolis_core::point::PointView {
                altitude: 180.0,
                pitch: -35.0,
                yaw: 20.0,
            }),
            meetup: None,
        };
        push(
            chat,
            sanne,
            0,
            "Sanne",
            "Where shall we have dinner Saturday?",
            None,
            0,
        );
        push(
            chat,
            sanne,
            ME,
            "You",
            "then the show after?",
            Some(point(
                53.2167,
                6.5608,
                "Stadsschouwburg",
                "then the show after?",
            )),
            2,
        );
        push(
            chat,
            sanne,
            0,
            "Sanne",
            "this café next door, 7 pm?",
            Some(point(
                53.2152,
                6.5660,
                "Turfsingel 84",
                "this café next door, 7 pm?",
            )),
            4,
        );
        push(chat, sanne, ME, "You", "Perfect, see you there", None, 6);
        // A snapshot, so a capture shows what a picture looks like in the
        // thread (`chat_pictures::DEMO_HASH` is drawn locally, never fetched).
        chat.receive_picture(
            sanne,
            0,
            "Sanne".into(),
            crate::systems::private_chat::picture_caption("the view from the Martinitoren!"),
            crate::systems::chat_pictures::demo_picture(),
        );
        if let Some(l) = chat
            .channels
            .iter_mut()
            .find(|c| c.key == sanne)
            .and_then(|c| c.lines.last_mut())
        {
            l.at_ms = at(8);
        }
        // A meet-up (`systems::meetup`), and Sanne sharing where she is, so the
        // card has a distance to show and the share row its Route.
        let mut vismarkt = point(53.2166, 6.5683, "Vismarkt", "halfway for both?");
        vismarkt.view = None;
        vismarkt.meetup = Some(cartopolis_core::point::Meetup {
            start_ms: crate::systems::meetup::default_start_ms(unix_ms()),
            going: false,
        });
        push(
            chat,
            sanne,
            0,
            "Sanne",
            "halfway for both?",
            Some(vismarkt),
            9,
        );
        chat.update_location(
            crate::systems::chat::SharedLocation {
                channel: sanne.into(),
                from: "Sanne".into(),
                latitude: 53.2232,
                longitude: 6.5720,
                accuracy_m: 8.0,
                until_ms: unix_ms() + 60 * 60_000,
            },
            unix_ms(),
        );
        let joris = "dm:10a5";
        chat.open_channel(joris, &crate::systems::private_chat::private_label("Joris"));
        push(
            chat,
            joris,
            0,
            "Joris",
            "Did you see the new bike bridge?",
            None,
            1,
        );
        let family = "dm:fa01";
        chat.open_channel(
            family,
            &crate::systems::private_chat::private_label("Family"),
        );
        push(chat, family, 0, "Oma", "Sunday lunch at ours?", None, 3);
        push(
            chat,
            "local",
            2,
            "Wieke",
            "Are you coming down to the Vismarkt?",
            None,
            5,
        );
        view.seen.insert(sanne.into(), 2);
    }
    view.demo = true;
    match screen {
        "chat" => {
            view.open_chat(sanne);
            chat.world = None;
            RightPanel::Chat
        }
        "meeting" => {
            seed_demo_meeting(chat);
            view.open_chat(DEMO_MEETING_CHAT);
            chat.world = None;
            RightPanel::Chat
        }
        // `+ → Plan a meeting` and `+ → Ask a poll`, half filled in.
        "plan" | "poll" => {
            seed_demo_meeting(chat);
            view.open_chat(DEMO_MEETING_CHAT);
            view.meeting_form = Some((
                DEMO_MEETING_CHAT.to_string(),
                if screen == "plan" {
                    MeetingForm::Plan {
                        title: "Street party wrap-up".into(),
                        start_ms: next_hour_ms(unix_ms()) + 24 * 3_600_000,
                        minutes: 45,
                        agenda: "What went well 10\nThe money 15\nNext year 20".into(),
                        text: String::new(),
                        repeat: Some((Every::Week, 1)),
                        // Nine weeks, so the capture shows Until stepped.
                        until_ms: Some(next_hour_ms(unix_ms()) + 57 * 24 * 3_600_000),
                    }
                } else {
                    MeetingForm::Poll {
                        question: "Which evening suits?".into(),
                        options: "Tuesday\nWednesday\nThursday".into(),
                        multiple: true,
                    }
                },
            ));
            chat.world = None;
            RightPanel::Chat
        }
        "switch" => {
            view.open = None;
            chat.world = None;
            RightPanel::None
        }
        "world" => {
            // Something Sanne has just said, to float over her marker
            // (`chat_people`); the thread's last line is yours.
            let said = "Great, see you at 7!";
            let already = chat
                .channels
                .iter()
                .any(|c| c.key == sanne && c.lines.iter().any(|l| l.body == said));
            if !already {
                chat.receive(sanne, 0, "Sanne".into(), said.into(), None);
            }
            view.open_chat(sanne);
            chat.world = Some(sanne.into());
            RightPanel::None
        }
        _ => {
            view.open = None;
            chat.world = None;
            RightPanel::Chat
        }
    }
}

const DEMO_MEETING_CHAT: &str = "dm:6d656574";

/// A group chat with what a meeting leaves in it, for `--ui chatmeeting`: a
/// plan with answers, a poll with votes, notes taken in the call, the record.
fn seed_demo_meeting(chat: &mut ChatState) {
    use cartopolis_core::meeting::{
        AgendaItem, MeetingObject, NoteKind, Plan, Poll, Record, RecordNote, RecordPoll, Rsvp,
    };
    let key = DEMO_MEETING_CHAT;
    if chat.channels.iter().any(|c| c.key == key) {
        return;
    }
    chat.open_channel(key, "Street party crew");
    let now = unix_ms();
    let say = |chat: &mut ChatState, id: u64, who: &str, m: MeetingObject| {
        chat.receive_meeting(key, id, who.into(), m.caption(), m);
    };
    let plan = "urn:uuid:demo-plan";
    say(
        chat,
        0,
        "Ann",
        MeetingObject::Plan(Plan {
            id: plan.into(),
            title: "Q4 planning".into(),
            start_ms: now + 5 * 60_000,
            end_ms: Some(now + 65 * 60_000),
            text: Some("Bring the numbers from the last party.".into()),
            agenda: vec![
                AgendaItem {
                    name: "Budget".into(),
                    minutes: 10,
                },
                AgendaItem {
                    name: "Stage and sound".into(),
                    minutes: 15,
                },
                AgendaItem {
                    name: "Who does what".into(),
                    minutes: 5,
                },
            ],
            // Weekly, so the capture shows a series and its next date.
            repeat: Some(cartopolis_core::meeting::Repeat {
                every: Every::Week,
                interval: 1,
                until_ms: None,
            }),
            skip: Vec::new(),
        }),
    );
    for (who, rsvp) in [
        ("Bob", Rsvp::Going),
        ("Carol", Rsvp::Maybe),
        ("Dave", Rsvp::Going),
    ] {
        say(
            chat,
            0,
            who,
            MeetingObject::Answer {
                plan: plan.into(),
                rsvp,
                date: cartopolis_core::meeting::AnswerFor::First,
            },
        );
    }
    let poll = "urn:uuid:demo-poll";
    say(
        chat,
        0,
        "Bob",
        MeetingObject::Poll(Poll {
            id: poll.into(),
            question: "Which day for the party?".into(),
            options: vec!["Friday".into(), "Saturday".into()],
            multiple: false,
        }),
    );
    for (who, id, day) in [
        ("Carol", 0, "Saturday"),
        ("Dave", 0, "Saturday"),
        ("You", 1, "Friday"),
    ] {
        say(
            chat,
            id,
            who,
            MeetingObject::Vote {
                poll: poll.into(),
                choices: vec![day.into()],
            },
        );
    }
    say(
        chat,
        0,
        "Ann",
        MeetingObject::Note {
            kind: NoteKind::Decision,
            text: "The gazebos stay in the budget".into(),
        },
    );
    say(
        chat,
        0,
        "Carol",
        MeetingObject::Note {
            kind: NoteKind::Action,
            text: "Carol books the stage by Friday".into(),
        },
    );
    say(
        chat,
        0,
        "Ann",
        MeetingObject::Record(Record {
            title: "Q4 planning".into(),
            start_ms: now.saturating_sub(40 * 60_000),
            end_ms: now.saturating_sub(5 * 60_000),
            people: vec!["ann".into(), "bob".into(), "carol".into(), "dave".into()],
            notes: vec![
                RecordNote {
                    kind: NoteKind::Decision,
                    text: "The gazebos stay in the budget".into(),
                    by: "ann".into(),
                },
                RecordNote {
                    kind: NoteKind::Action,
                    text: "Carol books the stage by Friday".into(),
                    by: "carol".into(),
                },
            ],
            polls: vec![RecordPoll {
                question: "Which day for the party?".into(),
                results: vec![("Friday".into(), 1), ("Saturday".into(), 2)],
            }],
            transcript: vec![
                ("bob".into(), "Shall we start with the budget?".into()),
                ("ann".into(), "Yes, the gazebos first.".into()),
            ],
        }),
    );
}

/// Undo [`seed_demo_chats`] for a capture that did not ask for it.
pub fn clear_demo_chats(chat: &mut ChatState, view: &mut ChatsView) {
    if view.demo {
        view.demo = false;
        view.open = None;
        chat.world = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meeting_form_posts_only_when_complete() {
        use cartopolis_core::meeting::MeetingObject;
        let now = 1_792_300_000_000;
        let mut plan = MeetingForm::plan(now);
        assert!(plan.object().is_none(), "a plan needs a title");
        let MeetingForm::Plan {
            title,
            start_ms,
            agenda,
            ..
        } = &mut plan
        else {
            unreachable!()
        };
        assert!(*start_ms > now && *start_ms - now <= 3_600_000);
        *title = " Q4 planning ".into();
        *agenda = "Budget 10\n\n- Wrap-up".into();
        let Some(MeetingObject::Plan(p)) = plan.object() else {
            panic!("a titled plan posts");
        };
        assert_eq!(p.title, "Q4 planning");
        assert_eq!(p.end_ms, Some(p.start_ms + 60 * 60_000));
        assert_eq!(p.agenda.len(), 2);
        assert_eq!(
            (p.agenda[0].minutes, p.agenda[1].name.as_str()),
            (10, "Wrap-up")
        );
        assert!(p.text.is_none());

        let mut poll = MeetingForm::poll();
        let MeetingForm::Poll {
            question, options, ..
        } = &mut poll
        else {
            unreachable!()
        };
        *question = "Which day?".into();
        *options = "Friday\n  \n".into();
        assert!(poll.object().is_none(), "one option is not a poll");
        if let MeetingForm::Poll { options, .. } = &mut poll {
            options.push_str("Saturday\n");
        }
        let Some(MeetingObject::Poll(p)) = poll.object() else {
            panic!("two options make a poll");
        };
        assert_eq!(
            p.options,
            vec!["Friday".to_string(), "Saturday".to_string()]
        );
    }

    #[test]
    fn a_chat_says_who_can_read_it() {
        assert!(subtitle(kind_of("dm:ab")).contains("end-to-end"));
        assert!(subtitle(kind_of("local")).contains("public"));
        assert!(subtitle(kind_of("geo:")).contains("public"));
        assert!(subtitle(ChatKind::Topic).contains("end-to-end"));
        assert_eq!(
            display_name(
                "dm:ab",
                &crate::systems::private_chat::private_label("Sanne")
            ),
            "Sanne"
        );
        assert_eq!(display_name("local", "Local"), "Here");
    }

    /// Unread counts only other people's lines, and only those after the chat
    /// was last looked at.
    #[test]
    fn your_own_lines_are_never_unread() {
        let mut chat = ChatState::default();
        chat.my_id = Some(1);
        chat.receive("dm:ab", 0, "Sanne".into(), "hi".into(), None);
        chat.receive("dm:ab", 1, "me".into(), "hello".into(), None);
        chat.receive("dm:ab", 0, "Sanne".into(), "dinner?".into(), None);
        let mut view = ChatsView::default();
        let lines = chat
            .channels
            .iter()
            .find(|c| c.key == "dm:ab")
            .unwrap()
            .lines
            .clone();
        assert_eq!(view.unread(&chat, "dm:ab", &lines), 2);
        view.seen.insert("dm:ab".into(), 2);
        assert_eq!(view.unread(&chat, "dm:ab", &lines), 1);
    }

    /// The "+" sheet's choices: a chat's opens its form, a community's asks
    /// for the community screen.
    #[test]
    fn a_new_sheet_choice_opens_its_form() {
        let mut kind = NewKind::Choose;
        let mut act = Actions::default();
        pick_new(NewChoice::Person, &mut kind, &mut act);
        assert_eq!(kind, NewKind::Person);
        pick_new(NewChoice::Group, &mut kind, &mut act);
        assert_eq!(kind, NewKind::Group);
        pick_new(NewChoice::FindCommunity, &mut kind, &mut act);
        assert!(act.community_find && !act.community_start);
        assert_eq!(kind, NewKind::Group, "a community's choice leaves the form");
    }

    /// A private line is sealed by `private_chat`; it must never leave as a
    /// plain `ChatSend`.
    #[test]
    fn a_private_line_goes_to_the_sealing_queue() {
        let mut chat = ChatState::default();
        send_line(&mut chat, None, false, "dm:ab", "secret".into());
        assert_eq!(
            chat.private_outbox,
            vec![("dm:ab".to_string(), "secret".to_string())]
        );
    }
}
