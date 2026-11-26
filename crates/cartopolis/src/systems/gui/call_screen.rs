//! The call screen: a call ringing, the call you are in, and the bar either
//! folds down to (`systems::calls` is the call itself; this decides what of
//! it shows, and `lib_call` draws it).
//!
//! Laid out the way Signal and WhatsApp lay theirs out, because that is the
//! screen everybody already knows how to use:
//!
//!  * **A call coming in takes the whole screen**: who, large, on a round
//!    initial with a slow pulse; what it is, small; Decline in red on the left
//!    and Answer in green on the right, at thumb height. Over a call you are
//!    already in it is a banner instead, so the call is not covered.
//!  * **In a call, the other person fills the screen** — their picture when
//!    their camera is on, cropped to fill (letterboxed only when an upright
//!    phone picture meets a wide desktop window), their initial when it is off —
//!    and your own picture is a small rounded window in the corner. Two or more
//!    others share the screen as a grid, with you as one of its tiles. While you are still ringing them, your own camera
//!    is the backdrop, as on a phone.
//!  * **The controls sit on the bottom edge** (camera, microphone, hang up) and
//!    the name and the timer on the top one; over a picture both fade out after
//!    a few seconds and come back at a tap.
//!  * **The arrow top-left, or Back, folds the screen down** to a green bar at
//!    the top of the app, with the microphone and hang-up on it, so the chats
//!    and the map stay usable during a call. Tapping the bar unfolds it. It is
//!    never *ended* by a gesture: hanging up and declining are buttons.
//!
//! The screen rings over whichever panel is open, and must be hang-up-able
//! from anywhere: the library draws it over every other screen.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::resources::{AuthIdentity, OnlineSession};
use crate::systems::calls::{camera_available, CallRequest, Calls, Ringing, SELF_TILE};
use crate::systems::chat::ChatState;
use crate::systems::icons;
use crate::systems::private_chat::channel_key;
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;
use cartopolis_core::{CallAction, CallSettings};

/// How long the controls stay up over a picture after the last touch.
const CONTROLS_HIDE_SECS: f32 = 4.0;
/// How long a notice ("Missed call from …") stays when nothing else is up.
const NOTICE_SECS: f32 = 6.0;
/// The bar a folded call or a ringing banner is, at most.
const BAR_MAX_WIDTH: f32 = 440.0;
/// Inset of the screen's controls from its edges. More than the map's
/// `OSD_MARGIN` at the bottom: a phone's gesture bar lives there.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const EDGE: f32 = 16.0;
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const BOTTOM_EDGE: f32 = 28.0;

// ─── The component library's edition (`lib_call`) ────────────────────────────
//
// This system decides everything — what rings, what shows, when the controls
// fade, who is large — and draws nothing: it publishes a `CallView` each
// frame, and the library's system draws it, with each video tile a `bevy:`
// image it writes the frames into. A tap there comes back as a `CallAct` and
// is done by `apply`.

/// The bridge to the library's call screen: what it draws, and the taps it
/// handed back.
#[derive(Resource, Default)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct CallScreenLib {
    /// What it draws this frame; `None` when nothing of a call shows.
    pub view: Option<CallView>,
    /// Taps from the library, done next frame by [`apply`].
    pub acts: Vec<CallAct>,
}

/// Everything of a call on screen, in UI points.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct CallView {
    /// The viewport.
    pub w: f32,
    pub h: f32,
    /// Whether pictures are shown at all: folded to the green bar they are
    /// left `fresh` for when the screen comes back.
    pub pictures: bool,
    /// The full screen.
    pub full: Option<FullView>,
    /// The sheet open over the full screen.
    pub sheet: Option<SheetView>,
    /// The folded call, ringing banners and the notice at the top.
    pub top: Option<TopView>,
}

/// What the full screen shows.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum FullView {
    /// A call coming in.
    Incoming {
        group: Vec<u8>,
        name: String,
        /// "Incoming call", or "Incoming call · <chat>".
        what: String,
        agent: bool,
    },
    /// Waiting for a host to let this device in.
    Waiting {
        title: String,
        /// Your own picture's width over its height, when your camera is on.
        own: Option<f32>,
        agent: bool,
    },
    /// The call you are in.
    InCall(Box<InCallView>),
}

/// One person, as a tile or a disc shows them.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct PersonView {
    pub id: u64,
    pub name: String,
    /// Their picture's width over its height, when their camera is on.
    pub picture: Option<f32>,
    pub speaking: bool,
    pub hand: Option<u32>,
    pub muted: bool,
    pub held: bool,
    pub host: bool,
    pub reaction: Option<String>,
}

/// The call you are in.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct InCallView {
    pub title: String,
    pub status: String,
    pub agent: bool,
    /// The others in your room, by name.
    pub people: Vec<PersonView>,
    /// You, as a tile.
    pub me: PersonView,
    /// Your own picture's width over its height.
    pub own: Option<f32>,
    /// A shared screen: whose, its width over its height, their name.
    pub screen: Option<(u64, f32, String)>,
    /// Who is large in Speaker view or the spotlight.
    pub big: Option<u64>,
    pub controls: bool,
    pub any_picture: bool,
    pub dock: DockView,
    /// The people button's badge: the count, and whether it is hands (gold).
    pub badge: Option<(usize, bool)>,
    pub chips: ChipsView,
    /// The caption on screen: who, what.
    pub caption: Option<(String, String)>,
}

/// The keys along the bottom.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct DockView {
    pub held: bool,
    pub muted: bool,
    /// The camera key, when this device has a camera: on or off.
    pub camera: Option<bool>,
    pub raised: bool,
}

/// What the meeting says under the title.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct ChipsView {
    /// "Recording · a, b".
    pub recording: Option<String>,
    /// Your screen going out: what it says.
    pub sharing: Option<&'static str>,
    /// "In Room 1 · 4:12 left", and whether the way back is offered.
    pub room: Option<(String, bool)>,
}

/// A tint for a word in a sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum Tint {
    Accent,
    Warn,
    Dim,
    Text,
}

/// The sheet open over the call screen, with what it holds.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum SheetView {
    More(MoreView),
    People(PeopleView),
    Leave { others: usize },
    Notes(NotesView),
}

impl SheetView {
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub fn kind(&self) -> Sheet {
        match self {
            SheetView::More(_) => Sheet::More,
            SheetView::People(_) => Sheet::People,
            SheetView::Leave { .. } => Sheet::Leave,
            SheetView::Notes(_) => Sheet::Notes,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct MoreView {
    pub speaker_view: bool,
    /// Noise suppression on.
    pub noise: bool,
    /// "Show your avatar instead", where there is a camera.
    pub avatar: Option<bool>,
    pub sharing: bool,
    pub hosting: Option<HostView>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct HostView {
    pub invited: Option<String>,
    pub settings: CallSettings,
    pub rooms_open: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct PeopleView {
    pub count: usize,
    pub hosting: bool,
    /// Waiting to join: who, and the line ("name · guest").
    pub waiting: Vec<(u64, String)>,
    pub hands: Vec<HandRow>,
    pub members: Vec<MemberRow>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct HandRow {
    pub participant: u64,
    /// Its place in the queue, from 1.
    pub place: usize,
    pub name: String,
    /// Your own hand (a non-host may lower only theirs).
    pub mine: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct MemberRow {
    pub participant: u64,
    /// "Ann · co-host".
    pub label: String,
    /// The state glyphs after it: held or muted, sharing.
    pub glyphs: Vec<&'static str>,
    pub you: bool,
    pub picked: bool,
    /// A host's actions on the picked person: icon, words, what they do.
    pub actions: Vec<(&'static str, String, Vec<CallAct>)>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct NotesView {
    pub agenda: Option<AgendaView>,
    /// The notes taken: kind (upper case), its tint, "text · sender".
    pub notes: Vec<(String, Tint, String)>,
    pub kind: cartopolis_core::meeting::NoteKind,
    pub hosting: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct AgendaView {
    pub title: String,
    pub items: Vec<AgendaRow>,
    /// "Start the agenda" or "Next: …", for a host.
    pub next: Option<(String, CallAct)>,
    /// Stop, for a host while it runs.
    pub stop: Option<CallAct>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct AgendaRow {
    /// "1. Welcome".
    pub label: String,
    pub current: bool,
    pub done: bool,
    /// The clock or the minutes on the right.
    pub right: Option<(String, Tint)>,
}

/// The folded call, the ringing banners and the notice, at the top.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct TopView {
    /// Their top edge, and the bar's width.
    pub top: f32,
    pub width: f32,
    pub folded: Option<FoldedView>,
    pub banners: Vec<BannerView>,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum FoldedView {
    /// The green bar.
    Bar {
        who: String,
        status: String,
        muted: bool,
    },
    /// The row of tiles over the map, and the keys under them (`controls`).
    Tiles {
        people: Vec<PersonView>,
        controls: bool,
        muted: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct BannerView {
    pub group: Vec<u8>,
    pub name: String,
    pub sub: String,
}

/// What a tap on the call screen asks for; done by [`apply`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum CallAct {
    /// Hand the call system this request.
    Request(CallRequest),
    /// Fold the screen down to the bar.
    Fold,
    /// Back to the full screen, controls up.
    Unfold,
    /// A tap on the picture: the controls up or down.
    ToggleControls,
    /// A key pressed: the controls stay up a while longer.
    Touch,
    /// Open a sheet, or close it (`None`, which forgets the picked person).
    Sheet(Option<Sheet>),
    /// The person a host picked in the people sheet.
    Pick(Option<u64>),
    /// The dock's microphone: says why when a host holds it.
    DockMute,
    /// The bar's microphone.
    ToggleMute,
    ToggleCamera,
    /// The dock's Leave: a host with others is asked.
    LeaveButton,
    ToggleSpeakerView,
    ToggleNoise,
    ToggleAvatar,
    CopyLink(String),
    SplitRooms(u32),
    /// Out of a breakout room, back to the main call.
    RoomBack,
    NoteKind(cartopolis_core::meeting::NoteKind),
    /// Add this note, of the kind chosen.
    AddNote(String),
    PostRecord,
    /// A ringing banner's body: unfold, when not in a call.
    BannerOpen,
    ClearNotice,
}

/// Do a tap on the call screen.
fn apply(
    act: CallAct,
    calls: &mut Calls,
    st: &mut CallScreenState,
    request: &mut Option<CallRequest>,
) {
    let now = || Some(web_time::Instant::now());
    match act {
        CallAct::Request(r) => *request = Some(r),
        CallAct::Fold => calls.minimized = true,
        CallAct::Unfold => {
            calls.minimized = false;
            st.controls = true;
            st.touched = now();
        }
        CallAct::ToggleControls => {
            st.controls = !st.controls;
            st.touched = now();
        }
        CallAct::Touch => st.touched = now(),
        CallAct::Sheet(sheet) => {
            st.sheet = sheet;
            if sheet.is_none() {
                st.picked = None;
            }
        }
        CallAct::Pick(p) => st.picked = p,
        CallAct::DockMute => {
            if calls.me().is_some_and(|m| m.held) {
                calls.notice = Some("A host muted you. Raise your hand to ask to talk.".into());
            } else {
                calls.muted = !calls.muted;
            }
        }
        CallAct::ToggleMute => calls.muted = !calls.muted,
        CallAct::ToggleCamera => calls.camera_on = !calls.camera_on,
        CallAct::LeaveButton => {
            // A host with others still in the call is asked: leave, or end it
            // for everyone. Anybody else just leaves, as on a phone.
            if calls.hosting() && !calls.others().is_empty() {
                st.sheet = Some(Sheet::Leave);
            } else {
                *request = Some(CallRequest::Leave);
            }
        }
        CallAct::ToggleSpeakerView => st.speaker_view = !st.speaker_view,
        CallAct::ToggleNoise => calls.keep_noise = !calls.keep_noise,
        CallAct::ToggleAvatar => {
            calls.avatar_video = !calls.avatar_video;
            if calls.avatar_video {
                calls.camera_on = true;
            }
        }
        CallAct::CopyLink(link) => crate::utils::clipboard::set_text(&link),
        CallAct::SplitRooms(n) => {
            *request = Some(CallRequest::Control(split_into_rooms(calls, n)));
        }
        CallAct::RoomBack => {
            if let Some(me) = calls.me() {
                *request = Some(CallRequest::Control(CallAction::MoveTo {
                    participant: me.participant,
                    room: 0,
                }));
            }
        }
        CallAct::NoteKind(k) => st.note_kind = Some(k),
        CallAct::AddNote(text) => {
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            let kind = st
                .note_kind
                .unwrap_or(cartopolis_core::meeting::NoteKind::Note);
            if let Some(group) = calls.active_group().map(<[u8]>::to_vec) {
                calls.meeting_out.push((
                    group,
                    cartopolis_core::meeting::MeetingObject::Note { kind, text },
                ));
            }
        }
        CallAct::PostRecord => calls.post_record_now(),
        CallAct::BannerOpen => {
            if calls.active_group().is_none() {
                calls.minimized = false;
            }
        }
        CallAct::ClearNotice => {
            calls.notice = None;
            st.notice = None;
        }
    }
}

/// The screen's own state between frames: timers and choices, nothing the
/// call needs.
#[derive(Default)]
pub struct CallScreenState {
    /// The sheet open over the call screen.
    sheet: Option<Sheet>,
    /// The person a host tapped in the people sheet.
    picked: Option<u64>,
    /// Speaker view: whoever talks is large. Off is the even grid.
    speaker_view: bool,
    /// Who is large in Speaker view, kept until somebody else talks.
    big: Option<u64>,
    /// Who was waiting at the last frame, to say when somebody new knocks.
    waiting_seen: Vec<u64>,
    /// The kind of note the notes sheet adds.
    note_kind: Option<cartopolis_core::meeting::NoteKind>,
    /// Calls that were ringing last frame, to unfold the screen for a new one.
    ringing_before: HashSet<Vec<u8>>,
    /// Last touch on the call screen, for hiding the controls over a picture.
    touched: Option<web_time::Instant>,
    /// Whether the controls are up over a picture.
    controls: bool,
    /// The notice on screen and when it appeared.
    notice: Option<(String, web_time::Instant)>,
}

/// One person on the call screen.
struct Person {
    id: u64,
    name: String,
    speaking: bool,
    /// A raised hand's place in the queue.
    hand: Option<u32>,
    muted: bool,
    /// A host muted them.
    held: bool,
    host: bool,
    /// The reaction floating over them.
    reaction: Option<String>,
}

/// What the screen is showing this frame, for its layout.
enum Mode<'a> {
    /// A call ringing here, nobody's call on this device.
    Incoming(&'a Ringing),
    /// The call this device is in.
    InCall,
}

/// Decide the call screen, the folded bar, ringing banners and call notices,
/// publish them for the library's edition (`lib_call`), and do the taps it
/// handed back.
#[allow(clippy::too_many_arguments)]
pub fn drive_call_screen(
    mut calls: ResMut<Calls>,
    identity: Res<AuthIdentity>,
    chat: Res<ChatState>,
    online: Option<Res<OnlineSession>>,
    mut st: Local<CallScreenState>,
    right_panel: Res<crate::resources::RightPanel>,
    sheet: Res<crate::systems::chat_overlay::ChatOverlay>,
    metrics: Res<UiMetrics>,
    mut lib: Option<ResMut<CallScreenLib>>,
) {
    calls.screen_shown = false;
    // The library's taps of last frame.
    if let Some(l) = lib.as_mut() {
        if !l.acts.is_empty() {
            let acts = std::mem::take(&mut l.acts);
            let mut request = None;
            for a in acts {
                apply(a, &mut calls, &mut st, &mut request);
            }
            if request.is_some() {
                calls.request = request;
            }
        }
    }
    if online.is_none() && !calls.demo {
        if let Some(l) = lib.as_mut() {
            l.view = None;
        }
        return;
    }
    let me = identity.username.clone().unwrap_or_default();
    let ringing = calls.ringing(&me);
    let active = calls.active_group().map(<[u8]>::to_vec);

    // A call that starts ringing unfolds the screen, whatever was folded before.
    let now_ringing: HashSet<Vec<u8>> = ringing.iter().map(|r| r.group.clone()).collect();
    if now_ringing.difference(&st.ringing_before).next().is_some() {
        calls.minimized = false;
        st.controls = true;
        st.touched = Some(web_time::Instant::now());
    }
    st.ringing_before = now_ringing;

    // Notices go on their own after a few seconds, or at a tap, as a phone's
    // "Call ended" does; nobody should have to find an OK to clear one.
    match (&calls.notice, &st.notice) {
        (Some(n), Some((shown, _))) if n == shown => {}
        (Some(n), _) => st.notice = Some((n.clone(), web_time::Instant::now())),
        (None, _) => st.notice = None,
    }
    if st
        .notice
        .as_ref()
        .is_some_and(|(_, at)| at.elapsed().as_secs_f32() > NOTICE_SECS)
    {
        calls.notice = None;
        st.notice = None;
    }

    if ringing.is_empty() && active.is_none() && calls.notice.is_none() {
        if let Some(l) = lib.as_mut() {
            l.view = None;
        }
        return;
    }

    // Folded over the map, the call is a row of tiles at the top of it
    // (`docs/mockups/chat-overlay`, v3); over a panel that is the whole
    // screen it is the green bar, and the bar shows no picture: the newest
    // is left waiting (`fresh`) for when the screen comes back. The
    // library's edition writes the pictures itself (`lib_call`).
    let map_shown = !(metrics.compact() && !right_panel.is(crate::resources::RightPanel::None));
    let tiles_shown = calls.minimized && active.is_some() && map_shown;

    let label_of = |group: &[u8]| {
        let key = channel_key(group);
        chat.channels
            .iter()
            .find(|c| c.key == key)
            .map(|c| crate::systems::private_chat::peer_of_label(&c.label).to_string())
    };
    let viewport = metrics.viewport_pts();

    // ── The full screen ────────────────────────────────────────────────────
    // The controls come up whenever the call screen does (a call starting, or
    // one unfolding), and a scripted capture keeps them up.
    if active.is_none() || calls.minimized {
        st.touched = None;
    } else if st.touched.is_none() || calls.demo {
        st.touched = Some(web_time::Instant::now());
        st.controls = true;
    }
    let mode = if calls.minimized {
        None
    } else if active.is_some() {
        Some(Mode::InCall)
    } else {
        ringing.first().map(Mode::Incoming)
    };
    if mode.is_some() {
        calls.screen_shown = true;
    }

    // ── The folded bar and the banners at the top ──────────────────────────
    let top = if chat.sharing.is_empty() {
        theme::OSD_MARGIN
    } else {
        theme::OSD_MARGIN + 44.0
    };
    let bar_width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(BAR_MAX_WIDTH);
    // Banners for calls ringing while the full screen shows something else:
    // over a call you are in, or every ringing call while folded.
    let banners: Vec<&Ringing> = match &mode {
        Some(Mode::Incoming(first)) => ringing.iter().filter(|r| r.group != first.group).collect(),
        _ => ringing.iter().collect(),
    };
    // The chat sheet open on this call's chat carries its keys in a strip;
    // the tiles then need only the way back.
    let strip_open = active.as_ref().is_some_and(|group| {
        let key = channel_key(group);
        sheet.composing && chat.channels.get(chat.active).is_some_and(|c| c.key == key)
    });
    let view = lib_view(
        &mut calls,
        &mut st,
        &chat,
        &label_of,
        mode.as_ref(),
        &banners,
        active.as_deref(),
        LibTop {
            top,
            width: bar_width,
            tiles_shown,
            strip_open,
        },
        viewport,
    );
    if let Some(l) = lib.as_mut() {
        if l.view.as_ref() != Some(&view) {
            l.view = Some(view);
        }
    }
}

/// A call's status line: the timer once it is up, or where it has got to.
fn status_line(calls: &Calls, others: usize) -> String {
    if calls.reconnecting() {
        "Reconnecting…".into()
    } else if others == 0 {
        "Ringing…".into()
    } else if calls.connected() {
        crate::systems::calls::call_clock(calls.elapsed_secs())
    } else {
        "Connecting…".into()
    }
}

/// The sheet open over the call screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sheet {
    /// Reactions, and the rest of the call's actions.
    More,
    /// Everybody in the call: hands, and a host's actions on a person.
    People,
    /// A host leaving: leave, or end it for everyone.
    Leave,
    /// Notes, decisions and actions taken in the call, and the agenda.
    Notes,
}

/// The reactions offered, in the order the row shows them.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const REACTIONS: [&str; 6] = ["👍", "👏", "😂", "❤", "🎉", "😮"];

/// Somebody on the call screen, from the call's roster.
fn person_of(calls: &Calls, id: u64, name: String) -> Person {
    let member = if id == SELF_TILE {
        calls.me()
    } else {
        calls.member(id)
    };
    Person {
        id,
        speaking: calls.speaking(id),
        hand: member.and_then(|m| m.hand),
        muted: member.is_some_and(|m| m.muted),
        held: member.is_some_and(|m| m.held),
        host: member.is_some_and(|m| m.role.hosts()),
        reaction: calls.reaction_of(id).map(|r| r.emoji.clone()),
        name,
    }
}

/// Who is large in Speaker view, if it is on: the hosts' spotlight first,
/// then whoever is talking, kept until somebody else talks. Needs two others,
/// or the one other person fills the screen anyway.
fn speaker_of(calls: &Calls, st: &mut CallScreenState, people: &[Person]) -> Option<u64> {
    if let Some(s) = calls.extras().spotlight {
        if people.iter().any(|p| p.id == s) {
            return Some(s);
        }
    }
    if !st.speaker_view || people.len() < 2 {
        return None;
    }
    if let Some(talking) = people.iter().find(|p| p.speaking) {
        st.big = Some(talking.id);
    }
    match st.big {
        Some(b) if people.iter().any(|p| p.id == b) => Some(b),
        _ => people.first().map(|p| p.id),
    }
}

/// Split everybody but the hosts into `n` rooms, in turn, after shuffling by
/// participant number and the clock (a host can move anybody after).
fn split_into_rooms(calls: &Calls, n: u32) -> CallAction {
    let mut people: Vec<u64> = calls
        .members()
        .iter()
        .filter(|m| !m.role.hosts())
        .map(|m| m.participant)
        .collect();
    let seed = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    people.sort_by_key(|p| p.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seed);
    CallAction::OpenRooms {
        names: (1..=n).map(|i| format!("Room {i}")).collect(),
        assign: people
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, i as u32 % n + 1))
            .collect(),
        minutes: 10,
        may_move: true,
    }
}

// ─── The view the library's edition draws ───────────────────────────────────

/// Where the top of the screen's pieces go, and which folded call shows.
struct LibTop {
    top: f32,
    width: f32,
    /// The tiles over the map rather than the green bar.
    tiles_shown: bool,
    /// The chat sheet's strip carries the microphone and hang-up.
    strip_open: bool,
}

/// Width over height of each picture held, by participant.
fn aspects<'a>(
    pictures: impl Iterator<Item = (u64, &'a mut crate::systems::calls::Picture)>,
) -> HashMap<u64, f32> {
    pictures
        .filter(|(_, p)| p.width > 0 && p.height > 0)
        .map(|(id, p)| (id, p.width as f32 / p.height as f32))
        .collect()
}

/// A person as the library draws them, with their picture's shape.
fn person_view(p: &Person, pictures: &HashMap<u64, f32>) -> PersonView {
    PersonView {
        id: p.id,
        name: p.name.clone(),
        picture: pictures.get(&p.id).copied(),
        speaking: p.speaking,
        hand: p.hand,
        muted: p.muted,
        held: p.held,
        host: p.host,
        reaction: p.reaction.clone(),
    }
}

/// What the library's edition draws this frame.
#[allow(clippy::too_many_arguments)]
fn lib_view(
    calls: &mut Calls,
    st: &mut CallScreenState,
    chat: &ChatState,
    label_of: &dyn Fn(&[u8]) -> Option<String>,
    mode: Option<&Mode>,
    banners: &[&Ringing],
    active: Option<&[u8]>,
    at: LibTop,
    viewport: Vec2,
) -> CallView {
    let tiles = aspects(calls.pictures_mut());
    let screens = aspects(calls.screens_mut());
    let mut sheet = None;
    let full = match mode {
        None => None,
        Some(Mode::Incoming(r)) => {
            let name = r.who.join(", ");
            let what = match label_of(&r.group) {
                Some(chat) if !chat.eq_ignore_ascii_case(&name) => {
                    format!("Incoming call · {chat}")
                }
                _ => "Incoming call".to_string(),
            };
            Some(FullView::Incoming {
                group: r.group.clone(),
                name,
                what,
                agent: r.agent,
            })
        }
        Some(Mode::InCall) => {
            let group = active.map(<[u8]>::to_vec).unwrap_or_default();
            let key = channel_key(&group);
            let lines = chat
                .channels
                .iter()
                .find(|c| c.key == key)
                .map_or(&[][..], |c| c.lines.as_slice());
            let chat_label = label_of(&group);
            if calls.waiting_to_join() {
                Some(FullView::Waiting {
                    title: chat_label.unwrap_or_else(|| "The call".into()),
                    own: tiles.get(&SELF_TILE).copied(),
                    agent: calls.with_agent(),
                })
            } else {
                let view = in_call_view(calls, st, chat_label, &tiles, &screens);
                let open = st.sheet;
                sheet = open.map(|s| sheet_view(s, &*calls, &*st, lines));
                Some(FullView::InCall(Box::new(view)))
            }
        }
    };

    let folded_call = calls.minimized && active.is_some();
    let notice = st.notice.as_ref().map(|(n, _)| n.clone());
    let top = if folded_call || !banners.is_empty() || notice.is_some() {
        let folded = folded_call.then(|| {
            if at.tiles_shown {
                let mut people: Vec<PersonView> = calls
                    .others()
                    .into_iter()
                    .take(3)
                    .map(|(id, name)| person_view(&person_of(calls, id, name), &tiles))
                    .collect();
                let mut me = person_of(calls, SELF_TILE, "You".into());
                me.muted = calls.muted;
                me.speaking = calls.speaking(SELF_TILE);
                people.push(person_view(&me, &tiles));
                FoldedView::Tiles {
                    people,
                    controls: !at.strip_open,
                    muted: calls.muted,
                }
            } else {
                let others: Vec<String> = calls.others().into_iter().map(|(_, n)| n).collect();
                let who = if others.is_empty() {
                    active
                        .and_then(|g| label_of(g))
                        .unwrap_or_else(|| "Calling".into())
                } else {
                    others.join(", ")
                };
                FoldedView::Bar {
                    status: status_line(calls, others.len()),
                    who,
                    muted: calls.muted,
                }
            }
        });
        let banners = banners
            .iter()
            .map(|r| {
                let name = r.who.join(", ");
                let sub = if active.is_some() {
                    "Incoming call · answering ends this one".to_string()
                } else {
                    match label_of(&r.group) {
                        Some(chat) if !chat.eq_ignore_ascii_case(&name) => {
                            format!("Incoming call · {chat}")
                        }
                        _ => "Incoming call".to_string(),
                    }
                };
                BannerView {
                    group: r.group.clone(),
                    name,
                    sub,
                }
            })
            .collect();
        Some(TopView {
            top: at.top,
            width: at.width,
            folded,
            banners,
            notice,
        })
    } else {
        None
    };

    CallView {
        w: viewport.x,
        h: viewport.y,
        pictures: !calls.minimized || at.tiles_shown,
        full,
        sheet,
        top,
    }
}

/// The call you are in: who is on the stage, whether the controls are up,
/// and what the meeting says.
fn in_call_view(
    calls: &mut Calls,
    st: &mut CallScreenState,
    chat_label: Option<String>,
    tiles: &HashMap<u64, f32>,
    screens: &HashMap<u64, f32>,
) -> InCallView {
    // Somebody new knocking is news to a host, wherever the people sheet is.
    let waiting = calls.extras().waiting;
    if calls.hosting() {
        if let Some(w) = waiting
            .iter()
            .find(|w| !st.waiting_seen.contains(&w.participant))
        {
            calls.notice = Some(format!(
                "{} is waiting to join. Admit them under People.",
                w.username
            ));
        }
    }
    st.waiting_seen = waiting.iter().map(|w| w.participant).collect();
    let mut people: Vec<Person> = calls
        .others()
        .into_iter()
        .map(|(id, name)| person_of(calls, id, name))
        .collect();
    let my_room = calls.me().map_or(0, |m| m.room);
    people.retain(|p| calls.member(p.id).map_or(0, |m| m.room) == my_room);
    people.sort_by(|a, b| a.name.cmp(&b.name));
    let own = tiles.get(&SELF_TILE).copied();
    let sharer = calls.sharer();
    let screen = sharer.and_then(|s| {
        screens.get(&s).map(|a| {
            let name = calls.name_of(s).unwrap_or("Somebody").to_string();
            (s, *a, name)
        })
    });
    let any_picture = screen.is_some()
        || people.iter().any(|p| tiles.contains_key(&p.id))
        || (people.is_empty() && own.is_some());
    if !any_picture || st.sheet.is_some() {
        st.controls = true;
    } else if st
        .touched
        .is_none_or(|t| t.elapsed().as_secs_f32() > CONTROLS_HIDE_SECS)
    {
        st.controls = false;
    }
    let status = status_line(calls, people.len());
    let title = if people.is_empty() || people.len() > 3 {
        chat_label.unwrap_or_else(|| "Calling…".into())
    } else {
        people
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut me = person_of(calls, SELF_TILE, "You".into());
    me.muted = calls.muted;
    me.speaking = calls.speaking(SELF_TILE);
    // Speaker view only without a shared screen, which takes the stage.
    let big = if screen.is_some() {
        None
    } else {
        speaker_of(calls, st, &people)
    };
    let hands = calls.members().iter().filter(|m| m.hand.is_some()).count()
        + if calls.hosting() {
            calls.extras().waiting.len()
        } else {
            0
        };
    let badge = if hands > 0 {
        Some((hands, true))
    } else {
        let n = calls.members().len();
        (n > 0).then_some((n, false))
    };
    let recorders: Vec<&str> = calls
        .members()
        .iter()
        .filter(|m| m.recording)
        .map(|m| m.username.as_str())
        .collect();
    let recording =
        (!recorders.is_empty()).then(|| format!("●  Recording · {}", recorders.join(", ")));
    let sharing = calls.sharing_screen().then(|| {
        if calls.screen_starting() {
            "Starting to share your screen…"
        } else {
            "You're sharing your screen"
        }
    });
    let extras = calls.extras();
    let room = match (extras.rooms.as_ref(), my_room > 0) {
        (Some(rooms), true) => {
            let name = rooms
                .names
                .get(my_room as usize - 1)
                .map_or("a breakout room", String::as_str);
            let left = rooms.until_ms.map(|until| {
                let now = web_time::SystemTime::now()
                    .duration_since(web_time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                let secs = until.saturating_sub(now) / 1000;
                format!(" · {}:{:02} left", secs / 60, secs % 60)
            });
            Some((
                format!("In {name}{}", left.unwrap_or_default()),
                rooms.may_move,
            ))
        }
        _ => None,
    };
    InCallView {
        title,
        status,
        agent: calls.with_agent(),
        people: people.iter().map(|p| person_view(p, tiles)).collect(),
        me: person_view(&me, tiles),
        own,
        screen,
        big,
        controls: st.controls,
        any_picture,
        dock: DockView {
            held: calls.me().is_some_and(|m| m.held),
            muted: calls.muted,
            camera: camera_available().then_some(calls.camera_on),
            raised: calls.me().is_some_and(|m| m.hand.is_some()),
        },
        badge,
        chips: ChipsView {
            recording,
            sharing,
            room,
        },
        caption: calls.caption_now(),
    }
}

/// The open sheet's contents.
fn sheet_view(
    sheet: Sheet,
    calls: &Calls,
    st: &CallScreenState,
    lines: &[crate::systems::chat::ChatLine],
) -> SheetView {
    match sheet {
        Sheet::More => SheetView::More(MoreView {
            speaker_view: st.speaker_view,
            noise: !calls.keep_noise,
            avatar: camera_available().then_some(calls.avatar_video),
            sharing: calls.sharing_screen(),
            hosting: calls.hosting().then(|| HostView {
                invited: calls.invited.clone(),
                settings: calls.call_settings(),
                rooms_open: calls.extras().rooms.is_some(),
            }),
        }),
        Sheet::People => SheetView::People(people_view(calls, st)),
        Sheet::Leave => SheetView::Leave {
            others: calls.others().len(),
        },
        Sheet::Notes => SheetView::Notes(notes_view(calls, st, lines)),
    }
}

/// The people sheet: who waits, whose hands are up, and everybody in the
/// call with a host's actions on the one picked.
fn people_view(calls: &Calls, st: &CallScreenState) -> PeopleView {
    use cartopolis_core::CallRole;
    let hosting = calls.hosting();
    let me = calls.me().map(|m| m.participant);
    let members = calls.members();
    let waiting = if hosting {
        calls
            .extras()
            .waiting
            .iter()
            .map(|w| {
                let guest = if w.guest { " · guest" } else { "" };
                (w.participant, format!("{}{guest}", w.username))
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut hands: Vec<&cartopolis_core::CallMember> =
        members.iter().filter(|m| m.hand.is_some()).collect();
    hands.sort_by_key(|m| m.hand);
    let hands = hands
        .iter()
        .enumerate()
        .map(|(i, m)| HandRow {
            participant: m.participant,
            place: i + 1,
            name: m.username.clone(),
            mine: Some(m.participant) == me,
        })
        .collect();
    let extras = calls.extras();
    let i_host = calls.me().is_some_and(|me| me.role == CallRole::Host);
    let members = members
        .iter()
        .map(|m| {
            let you = Some(m.participant) == me;
            let role = match m.role {
                CallRole::Host => " · host",
                CallRole::CoHost => " · co-host",
                CallRole::Member => "",
            };
            let name = if you { "You" } else { m.username.as_str() };
            let mut glyphs = Vec::new();
            if m.held {
                glyphs.push(icons::LOCK);
            } else if m.muted {
                glyphs.push(icons::MIC_OFF);
            }
            if m.sharing {
                glyphs.push(icons::SCREEN_SHARE);
            }
            let picked = st.picked == Some(m.participant);
            let mut actions: Vec<(&'static str, String, Vec<CallAct>)> = Vec::new();
            if hosting && picked {
                let participant = m.participant;
                let control = |a: CallAction| vec![CallAct::Request(CallRequest::Control(a))];
                if m.held {
                    actions.push((
                        icons::MIC,
                        "Let talk".into(),
                        control(CallAction::AllowTalk { participant }),
                    ));
                } else {
                    actions.push((
                        icons::MIC_OFF,
                        "Mute".into(),
                        control(CallAction::Mute { participant }),
                    ));
                }
                if m.sharing {
                    actions.push((
                        icons::SCREEN_SHARE,
                        "Stop their screen".into(),
                        control(CallAction::StopSharing { participant }),
                    ));
                }
                if m.role != CallRole::Host {
                    let (word, role) = match m.role {
                        CallRole::Member => ("Make co-host", CallRole::CoHost),
                        _ => ("Make a member again", CallRole::Member),
                    };
                    actions.push((
                        icons::HOST,
                        word.into(),
                        control(CallAction::Role { participant, role }),
                    ));
                }
                if i_host {
                    actions.push((
                        icons::STAR,
                        "Hand the call over".into(),
                        control(CallAction::Role {
                            participant,
                            role: CallRole::Host,
                        }),
                    ));
                }
                let spotlit = extras.spotlight == Some(participant);
                let (word, target) = if spotlit {
                    ("Stop the spotlight", None)
                } else {
                    ("Spotlight for everyone", Some(participant))
                };
                actions.push((
                    icons::STAR,
                    word.into(),
                    control(CallAction::Spotlight {
                        participant: target,
                    }),
                ));
                if let Some(rooms) = &extras.rooms {
                    for (i, name) in std::iter::once("the main call")
                        .chain(rooms.names.iter().map(String::as_str))
                        .enumerate()
                    {
                        if i as u32 != m.room {
                            actions.push((
                                icons::GROUP,
                                format!("Move to {name}"),
                                control(CallAction::MoveTo {
                                    participant,
                                    room: i as u32,
                                }),
                            ));
                        }
                    }
                }
                let mut remove = control(CallAction::Remove { participant });
                remove.push(CallAct::Pick(None));
                actions.push((icons::PERSON_REMOVE, "Remove from the call".into(), remove));
            }
            MemberRow {
                participant: m.participant,
                label: format!("{name}{role}"),
                glyphs,
                you,
                picked,
                actions,
            }
        })
        .collect();
    PeopleView {
        count: calls.members().len(),
        hosting,
        waiting,
        hands,
        members,
    }
}

/// The notes sheet: the agenda of the plan this call belongs to, and the
/// notes, decisions and actions taken in it.
fn notes_view(
    calls: &Calls,
    st: &CallScreenState,
    lines: &[crate::systems::chat::ChatLine],
) -> NotesView {
    use cartopolis_core::meeting::{MeetingObject, NoteKind, Plan};
    let since = calls.started_ms().unwrap_or(0).saturating_sub(60_000);
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let hosting = calls.hosting();
    let start = calls.started_ms().unwrap_or(now);
    let plan: Option<&Plan> = lines
        .iter()
        .filter_map(|l| match &l.meeting {
            Some(MeetingObject::Plan(p))
                if p.start_ms <= start + 15 * 60_000 && start <= p.start_ms + 3 * 3_600_000 =>
            {
                Some(p)
            }
            _ => None,
        })
        .min_by_key(|p| p.start_ms.abs_diff(start));
    let agenda = plan.map(|plan| {
        let at = calls.agenda.clone().filter(|a| a.plan == plan.id);
        let items = plan
            .agenda
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let current = at.as_ref().is_some_and(|a| a.index == i);
                let done = at.as_ref().is_some_and(|a| i < a.index);
                let right = if current {
                    let spent =
                        now.saturating_sub(at.as_ref().map_or(now, |a| a.started_ms)) / 1000;
                    let over = item.minutes > 0 && spent > u64::from(item.minutes) * 60;
                    let text = if item.minutes > 0 {
                        format!("{}:{:02} / {}:00", spent / 60, spent % 60, item.minutes)
                    } else {
                        format!("{}:{:02}", spent / 60, spent % 60)
                    };
                    Some((text, if over { Tint::Warn } else { Tint::Accent }))
                } else if item.minutes > 0 {
                    Some((format!("{} min", item.minutes), Tint::Dim))
                } else {
                    None
                };
                AgendaRow {
                    label: format!("{}. {}", i + 1, item.name),
                    current,
                    done,
                    right,
                }
            })
            .collect();
        let (next, stop) = if hosting && !plan.agenda.is_empty() {
            let n = at.as_ref().map_or(0, |a| a.index + 1);
            let next = (n < plan.agenda.len()).then(|| {
                let label = if at.is_some() {
                    format!("Next: {}", plan.agenda[n].name)
                } else {
                    "Start the agenda".to_string()
                };
                (
                    label,
                    CallAct::Request(CallRequest::Agenda(plan.id.clone(), Some(n))),
                )
            });
            let stop = at
                .is_some()
                .then(|| CallAct::Request(CallRequest::Agenda(plan.id.clone(), None)));
            (next, stop)
        } else {
            (None, None)
        };
        AgendaView {
            title: plan.title.clone(),
            items,
            next,
            stop,
        }
    });
    let notes = lines
        .iter()
        .filter(|l| l.at_ms >= since)
        .filter_map(|l| match &l.meeting {
            Some(MeetingObject::Note { kind, text }) => {
                let tint = match kind {
                    NoteKind::Decision => Tint::Accent,
                    NoteKind::Action => Tint::Warn,
                    NoteKind::Note => Tint::Dim,
                };
                Some((
                    kind.label().to_uppercase(),
                    tint,
                    format!("{text} · {}", l.sender),
                ))
            }
            _ => None,
        })
        .collect();
    NotesView {
        agenda,
        notes,
        kind: st.note_kind.unwrap_or(NoteKind::Note),
        hosting,
    }
}
