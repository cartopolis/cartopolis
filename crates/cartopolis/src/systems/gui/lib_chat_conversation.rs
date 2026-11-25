//! One chat's Chat view on the component library, inside the Chats panel's
//! shell (`lib_chats`): egui's edition is `chats::chat_screen` and what it
//! calls, and both editions share its logic.
//!
//! - **The header**: back, the chat's disc, its name over one line of status
//!   (a location being shared, the topic's community, or who can read it),
//!   and the call key (`chats::call_button`). Under it the chat's own
//!   Chat | World.
//! - **The thread**: day chips, bubbles (yours right in the accent's soft
//!   tint, theirs left on the raised surface, a run by one speaker held
//!   close and the next speaker set apart), point cards, pictures, meeting
//!   and meet-up cards, and the menu behind a speaker's name
//!   (`chats::person_act`, as `chat::person_menu` does for egui).
//! - **The composer**: what is on its way (pictures, a picture being picked),
//!   the meet-up being proposed, the location shares, the "+" menu
//!   (`chats::attach_entries` / `pick_attach`), and the field with send — or,
//!   in its place, the meeting or poll form (`chats::shift_start`,
//!   `pick_repeat`, `step_until`, `MeetingForm::object`).
//!
//! Every tap fills the frame's `chats::Actions` (or changes `ChatsView` as
//! egui's drawing does) through `ChatsLive::on`; `chats::settle` does the
//! rest. What is shown is mirrored from the world every frame into the
//! signals of [`ConversationLive`], each written only when it moved.
//!
//! What the library cannot draw yet: a picture is a tile naming it, which
//! opens egui's full-size viewer (`a.view_picture`); a point's card has the
//! pin where its picture would go. The thread does not stick to its bottom
//! (the renderer has no scroll action), so it shows the last [`PAGE`] items
//! and a way to the earlier ones.

use std::cell::RefCell;
use std::rc::Rc;

use cartopolis_core::meeting::{Every, MeetingObject, NoteKind, Plan, Poll, Rsvp};
use cartopolis_core::point::Point;
use mithril::prelude::*;

use crate::systems::calls::CallRequest;
use crate::systems::chat::{ChatLine, ChatState, PersonAction};
use crate::systems::chat_pictures::PictureRef;
use crate::systems::chats::{
    self, Attach, CallButton, ChatsScreen, MeetingForm, TopicContext, UntilStep,
};
use crate::systems::discs::{kind_of, ChatKind};
use crate::systems::icons;
use crate::systems::lib_chats::{
    back, disc, group, header, icon, note, ChatsLive, ChatsWorld, Typed,
};
use crate::utils::clock::unix_ms;

/// How many of the thread's last items show before "Show earlier messages".
const PAGE: usize = 40;

/// The longest edge of a picture's tile, px: `chats::PICTURE_MAX_PT`.
const PICTURE_MAX: f32 = 240.0;

/// A raised block in the composer: the meet-up, the "+" menu, the forms.
const PANEL: &str = "display: flex; flex-direction: column; gap: 12px; padding: 16px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface)";

/// A small label over a row of choices.
const LABEL: &str = "color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 600";

// ─── What is drawn ───────────────────────────────────────────────────────────

/// What the header's second line says.
#[derive(Clone, PartialEq)]
enum Status {
    /// Somebody shares where their phone is: "Sanne shares … · 12 min left".
    Location(String),
    /// A topic: its community's line, which opens the community.
    Topic(String, uuid::Uuid),
    /// Who can read it (`chats::subtitle`).
    Plain(ChatKind),
}

#[derive(Clone, PartialEq)]
struct HeadV {
    name: String,
    kind: ChatKind,
    status: Status,
    call: CallButton,
}

/// What the person menu offers besides message, block and report.
#[derive(Clone, PartialEq, Default)]
struct Extras {
    /// The chat is a topic: its community's name.
    community: Option<String>,
    /// This account moderates it: the topic's name.
    moderate_topic: Option<String>,
    /// This account may suspend and delete accounts.
    admin: bool,
}

#[derive(Clone, PartialEq, Default)]
struct ThreadV {
    items: Vec<Item>,
    /// Nothing said yet: the line under "Nothing here yet".
    empty: Option<&'static str>,
    /// Your request to them is not accepted yet.
    request: Option<String>,
    extras: Extras,
}

/// One thing in the thread.
#[derive(Clone, PartialEq)]
enum Item {
    /// "Earlier messages, shared by …".
    Shared(String),
    Day(String),
    /// A line a moderator deleted.
    Removed,
    Line(LineV),
    /// "Sanne is going", under a meet-up.
    Going(String),
    Meeting(MeetingV),
    Meetup(MeetupV),
}

/// A line by somebody: a bubble, a picture or a point.
#[derive(Clone, PartialEq)]
struct LineV {
    /// `ChatLine::seq`: which line a person menu is open on.
    seq: u64,
    /// px above it: close in one speaker's run, apart at a new speaker.
    gap: u32,
    mine: bool,
    /// The speaker's name over it, in a group (with the distance in Here).
    who: Option<String>,
    sender: String,
    /// Its place in a topic's log, for the moderators' acts.
    log_seq: Option<u64>,
    body: String,
    time: Option<String>,
    what: What,
}

#[derive(Clone, PartialEq)]
enum What {
    Text,
    Picture {
        picture: PictureRef,
        caption: Option<String>,
    },
    Point(PointV),
}

#[derive(Clone, PartialEq)]
struct PointV {
    point: Point,
    /// The number its pin carries on the World view.
    number: usize,
    title: String,
    sub: String,
}

#[derive(Clone, PartialEq)]
enum MeetingV {
    Plan(PlanV),
    /// "You skipped … on …", what a planner's update changed.
    Change(String),
    Poll(PollV),
    Note {
        kind: NoteKind,
        text: String,
    },
    Record(RecordV),
}

#[derive(Clone, PartialEq)]
struct PlanV {
    /// As its planner last left it.
    plan: Plan,
    /// The date the card is about.
    date: u64,
    /// "Meeting · Today 18:00".
    when: String,
    /// "18:00–19:00".
    hours: String,
    rule: Option<String>,
    going: Option<String>,
    /// About to start or going on: the accent's edge.
    live: bool,
    /// Live, in a private chat: Join.
    join: bool,
    /// Each answer and how many gave it, while the date is not over.
    answers: Option<Vec<(Rsvp, usize)>>,
    mine: Option<Rsvp>,
    /// Your own series: the date's short name, and whether it is its last.
    own: Option<(String, bool)>,
}

#[derive(Clone, PartialEq)]
struct PollV {
    poll: Poll,
    /// Each option, how many chose it, and whether you did.
    options: Vec<(String, usize, bool)>,
    voters: usize,
    mine: Vec<String>,
}

#[derive(Clone, PartialEq)]
struct RecordV {
    title: String,
    /// "18:00–19:02 · 62 min · sanne, jeroen".
    line: String,
    /// "Decisions", "Actions", "Notes" and each poll's results.
    sections: Vec<(String, Vec<String>)>,
    transcript: Vec<String>,
    /// "Copy as text".
    copy: String,
}

#[derive(Clone, PartialEq)]
struct MeetupV {
    point: Point,
    place: String,
    title: String,
    /// "Today 18:00 · suggested by Sanne".
    sub: String,
    people: Vec<(String, String)>,
    /// You are going.
    going: bool,
    /// What "I'm going" shares.
    note: String,
}

/// The meet-up being proposed, above the field.
#[derive(Clone, PartialEq)]
struct DraftV {
    place: String,
    halfway: bool,
    when: String,
}

/// The "+" menu, while it is open.
#[derive(Clone, PartialEq)]
struct AttachV {
    public: bool,
    entries: Vec<Attach>,
    share_open: bool,
    sharing: bool,
}

/// Everything above the field.
#[derive(Clone, PartialEq, Default)]
struct FootV {
    key: String,
    /// Pictures on their way out.
    sending: usize,
    /// A picture is being picked for this chat.
    waiting: bool,
    error: Option<String>,
    draft: Option<DraftV>,
    /// You share your location here: minutes left.
    sharing: Option<u64>,
    /// Who shares theirs: (who, lat, lng).
    locations: Vec<(String, f64, f64)>,
    attach: Option<AttachV>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FormKind {
    Plan,
    Poll,
}

/// The form's values that are not typed.
#[derive(Clone, PartialEq, Default)]
struct FormV {
    when: String,
    minutes: u32,
    repeat: Option<(Every, u32)>,
    /// "Thu 29 Oct · 4 times"; `None` for no end.
    until: Option<String>,
    /// It can be posted.
    ready: bool,
}

/// A choice in the menu behind a speaker's name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    Message,
    Block,
    ReportMods,
    Report,
    DeleteAll,
    RemoveTopic,
    RemoveCommunity,
    Suspend,
    DeleteAccount,
}

impl Pick {
    fn action(self, body: &str) -> PersonAction {
        match self {
            Pick::Message => PersonAction::Message,
            Pick::Block => PersonAction::Block,
            Pick::ReportMods => PersonAction::ReportToModerators(Some(body.to_string())),
            Pick::Report => PersonAction::Report(Some(body.to_string())),
            Pick::DeleteAll => PersonAction::DeleteForEveryone,
            Pick::RemoveTopic => PersonAction::RemoveFromTopic,
            Pick::RemoveCommunity => PersonAction::RemoveFromCommunity,
            Pick::Suspend => PersonAction::Suspend,
            Pick::DeleteAccount => PersonAction::DeleteAccount,
        }
    }

    /// The acts that change what somebody else can do arm first, as
    /// `theme::confirm_button` does in egui's menu: the question shown.
    fn confirm(self) -> Option<&'static str> {
        match self {
            Pick::Block => Some("Block them?"),
            Pick::DeleteAll => Some("Delete it for everyone?"),
            Pick::Suspend => Some("Suspend them?"),
            _ => None,
        }
    }
}

// ─── The live state ──────────────────────────────────────────────────────────

/// The conversation's own signals, kept in `ChatsLive::conversation`.
#[derive(Clone)]
pub(crate) struct ConversationLive {
    head: Signal<Option<HeadV>>,
    thread: Signal<ThreadV>,
    foot: Signal<FootV>,
    /// The "+" is open: its glyph is ✕.
    attach_open: Signal<bool>,
    /// Something is typed: send can be pressed.
    ready: Signal<bool>,
    form_kind: Signal<Option<FormKind>>,
    /// How far the on-screen keyboard reaches up (`KeyboardInset`): the
    /// composer lifts by it, so what is typed stays in sight.
    lift: Signal<f32>,
    form: Signal<FormV>,
    /// `ChatsView::draft`.
    draft: Typed,
    /// The forms' text fields, bridged to `ChatsView::meeting_form`.
    title: Typed,
    agenda: Typed,
    text: Typed,
    question: Typed,
    options: Typed,
    multiple: Signal<bool>,
    /// The line (`ChatLine::seq`) whose person menu is open.
    menu: Signal<Option<u64>>,
    /// The menu's choice waiting for its second tap.
    armed: Signal<Option<Pick>>,
    /// How many of the thread's last items show.
    shown: Signal<usize>,
    /// The chat the state above is for.
    open: Rc<RefCell<String>>,
    /// Text to put on the clipboard, which is egui's (`take_copy`).
    copy: Rc<RefCell<Option<String>>>,
}

impl ConversationLive {
    /// Made when the panel mounts, from the world as it is then.
    pub(crate) fn new(w: &ChatsWorld) -> Self {
        ConversationLive {
            head: signal(None),
            thread: signal(ThreadV::default()),
            foot: signal(FootV::default()),
            attach_open: signal(false),
            ready: signal(false),
            form_kind: signal(None),
            lift: signal(0.0),
            form: signal(FormV::default()),
            draft: Typed::new(&w.view.draft),
            title: Typed::new(""),
            agenda: Typed::new(""),
            text: Typed::new(""),
            question: Typed::new(""),
            options: Typed::new(""),
            multiple: signal(false),
            menu: signal(None),
            armed: signal(None),
            shown: signal(PAGE),
            open: Rc::new(RefCell::new(String::new())),
            copy: Rc::new(RefCell::new(None)),
        }
    }
}

/// Text the conversation asked to copy, for the system, which has egui's
/// clipboard.
pub(crate) fn take_copy(live: &ChatsLive) -> Option<String> {
    live.conversation.copy.borrow_mut().take()
}

/// Write `value` into `s` only when it differs: a write re-runs every view
/// that reads it.
fn set<T: Clone + PartialEq + 'static>(s: &Signal<T>, value: T) {
    if s.get_untracked() != value {
        s.set(value);
    }
}

// ─── The world → the signals ─────────────────────────────────────────────────

/// The world → [`ConversationLive`], every frame the panel is mounted.
pub(crate) fn mirror(live: &ChatsLive, w: &mut ChatsWorld, now: u64) {
    let c = &live.conversation;
    let ChatsScreen::Chat(key) = w.screen() else {
        return;
    };
    if *c.open.borrow() != key {
        *c.open.borrow_mut() = key.clone();
        set(&c.shown, PAGE);
        set(&c.menu, None);
        set(&c.armed, None);
    }
    let lift = w.keyboard.as_ref().map_or(0.0, |k| k.0);
    if (c.lift.get_untracked() - lift).abs() > 0.5 {
        c.lift.set(lift);
    }
    // As egui's: a chat that is not there (left, or gone) goes back.
    if !w.chat.channels.iter().any(|ch| ch.key == key) {
        (live.later(|_, a| a.back = true))();
        return;
    }
    pull(c, w, &key);
    c.draft.follow(&w.view.draft);
    let form = w
        .view
        .meeting_form
        .as_ref()
        .filter(|(k, _)| *k == key)
        .map(|(_, f)| f.clone());
    follow_form(c, form.as_ref());
    set(
        &c.form_kind,
        form.as_ref().map(|f| match f {
            MeetingForm::Plan { .. } => FormKind::Plan,
            MeetingForm::Poll { .. } => FormKind::Poll,
        }),
    );
    set(
        &c.form,
        form.as_ref().map(|f| form_v(f, now)).unwrap_or_default(),
    );
    if let Some(MeetingForm::Poll { multiple, .. }) = &form {
        set(&c.multiple, *multiple);
    }
    set(&c.head, head_v(w, &key, now));
    set(&c.thread, thread_v(w, &key, now));
    set(&c.foot, foot_v(w, &key, now));
    set(&c.attach_open, w.view.attach);
    set(&c.ready, !w.view.draft.trim().is_empty());
}

/// What was typed into the fields since the last frame → the world, as
/// egui's `TextEdit`s write it. Also run by a tap that reads them (send,
/// post), so a key pressed in the tap's frame is not lost.
fn pull(c: &ConversationLive, w: &mut ChatsWorld, key: &str) {
    if let Some(v) = c.draft.typed(&w.view.draft) {
        w.view.draft = v;
    }
    let typed: [Option<String>; 3] = match w.view.meeting_form.as_ref() {
        Some((
            k,
            MeetingForm::Plan {
                title,
                agenda,
                text,
                ..
            },
        )) if k.as_str() == key => [
            c.title.typed(title),
            c.agenda.typed(agenda),
            c.text.typed(text),
        ],
        Some((
            k,
            MeetingForm::Poll {
                question, options, ..
            },
        )) if k.as_str() == key => [c.question.typed(question), c.options.typed(options), None],
        _ => return,
    };
    if typed.iter().all(Option::is_none) {
        return;
    }
    let Some((_, form)) = w.view.meeting_form.as_mut() else {
        return;
    };
    let [a, b, d] = typed;
    match form {
        MeetingForm::Plan {
            title,
            agenda,
            text,
            ..
        } => {
            for (to, v) in [(title, a), (agenda, b), (text, d)] {
                if let Some(v) = v {
                    *to = v;
                }
            }
        }
        MeetingForm::Poll {
            question, options, ..
        } => {
            for (to, v) in [(question, a), (options, b)] {
                if let Some(v) = v {
                    *to = v;
                }
            }
        }
    }
}

/// The open form's text → its fields, where it moved (or "" with no form).
fn follow_form(c: &ConversationLive, form: Option<&MeetingForm>) {
    let (t, a, x, q, o) = match form {
        Some(MeetingForm::Plan {
            title,
            agenda,
            text,
            ..
        }) => (title.as_str(), agenda.as_str(), text.as_str(), "", ""),
        Some(MeetingForm::Poll {
            question, options, ..
        }) => ("", "", "", question.as_str(), options.as_str()),
        None => ("", "", "", "", ""),
    };
    c.title.follow(t);
    c.agenda.follow(a);
    c.text.follow(x);
    c.question.follow(q);
    c.options.follow(o);
}

fn form_v(f: &MeetingForm, now: u64) -> FormV {
    let ready = f.object().is_some();
    match f {
        MeetingForm::Plan {
            start_ms,
            minutes,
            repeat,
            until_ms,
            ..
        } => FormV {
            when: crate::systems::meetup::when_label(
                *start_ms,
                now,
                chats::local_offset_s(*start_ms),
            ),
            minutes: *minutes,
            repeat: *repeat,
            until: chats::until_at(*start_ms, *repeat, *until_ms).map(|(i, d)| {
                format!(
                    "{} · {} times",
                    crate::systems::calendar::short_day(d),
                    i + 1
                )
            }),
            ready,
        },
        MeetingForm::Poll { .. } => FormV {
            ready,
            ..FormV::default()
        },
    }
}

fn head_v(w: &ChatsWorld, key: &str, now: u64) -> Option<HeadV> {
    let channel = w.chat.channels.iter().find(|c| c.key == key)?;
    let topic = TopicContext::of(&w.communities, key);
    let kind = if topic.is_some() {
        ChatKind::Topic
    } else {
        kind_of(key)
    };
    let name = topic.as_ref().map_or_else(
        || chats::display_name(key, &channel.label),
        |t| t.name.clone(),
    );
    let status = if let Some(l) = w.chat.locations.iter().find(|l| l.channel == key) {
        Status::Location(format!(
            "{} shares their location · {} min left",
            l.from,
            l.until_ms.saturating_sub(now) / 60_000 + 1
        ))
    } else if let Some(t) = &topic {
        Status::Topic(t.sub.clone(), t.community)
    } else {
        Status::Plain(kind)
    };
    Some(HeadV {
        name,
        kind,
        status,
        call: chats::call_button(&w.calls, key),
    })
}

/// The thread as `chats::chat_screen` walks it.
fn thread_v(w: &ChatsWorld, key: &str, now: u64) -> ThreadV {
    let chat: &ChatState = &w.chat;
    let Some(channel) = chat.channels.iter().find(|c| c.key == key) else {
        return ThreadV::default();
    };
    let topic = TopicContext::of(&w.communities, key);
    let kind = if topic.is_some() {
        ChatKind::Topic
    } else {
        kind_of(key)
    };
    let lines: Vec<ChatLine> = channel
        .lines
        .iter()
        .filter(|l| !chat.is_blocked(&l.sender))
        .cloned()
        .collect();
    let group = kind != ChatKind::Private || !chats::channel_is_direct(chat, key);
    let peer = chat
        .direct
        .iter()
        .find(|(_, k)| k.as_str() == key)
        .map(|(p, _)| p.clone());
    let request = peer.filter(|p| w.contacts.sent_to(p)).map(|peer| {
        format!(
            "Request sent. {peer} sees your first message in their requests; you can \
             write more once they accept."
        )
    });
    let empty = lines.is_empty().then_some(match kind {
        ChatKind::Private => "Say hello, or send a place with + → Send this view.",
        ChatKind::Here => "What people near your avatar say appears here and over their heads.",
        _ => "Messages appear here.",
    });
    let mut items = Vec::new();
    let mut number = 0usize;
    let mut last_day = String::new();
    let mut told_shared = false;
    // The speaker of the run the next line may continue: (who, mine).
    let mut run: Option<(String, bool)> = None;
    for (i, line) in lines.iter().enumerate() {
        if let (Some(by), false) = (&line.shared_by, told_shared) {
            told_shared = true;
            run = None;
            items.push(Item::Shared(format!(
                "Earlier messages, shared by {by} when you joined"
            )));
        }
        if line.at_ms > 0 {
            let day = chats::day_label(line.at_ms, now);
            if day != last_day {
                items.push(Item::Day(day.clone()));
                last_day = day;
                run = None;
            }
        }
        if let Some(meeting) = &line.meeting {
            if meeting.is_line() {
                if let Some(m) = meeting_v(chat, key, line, i, meeting, &lines, now) {
                    items.push(Item::Meeting(m));
                    run = None;
                }
            }
            continue;
        }
        match line.point.as_ref().and_then(|p| p.meetup) {
            Some(m) if m.going => {
                items.push(Item::Going(if chat.is_mine(line) {
                    "You are going".to_string()
                } else {
                    format!("{} is going", line.sender)
                }));
                run = None;
                continue;
            }
            Some(_) => {
                number += 1;
                if let Some(v) = meetup_v(chat, key, line, &lines, now) {
                    items.push(Item::Meetup(v));
                }
                run = None;
                continue;
            }
            None => {}
        }
        let n = line.point.as_ref().map(|_| {
            number += 1;
            number
        });
        if line.removed {
            items.push(Item::Removed);
            run = None;
            continue;
        }
        let mine = chat.is_mine(line);
        let gap = if run
            .as_ref()
            .is_some_and(|(s, m)| *s == line.sender && *m == mine)
        {
            4
        } else {
            14
        };
        run = Some((line.sender.clone(), mine));
        let who = (group && !mine).then(|| match line.distance {
            Some(d) => format!("{} · {:.0} m", line.sender, d),
            None => line.sender.clone(),
        });
        let what = if let Some(picture) = &line.picture {
            What::Picture {
                picture: picture.clone(),
                caption: crate::systems::private_chat::caption_of(&line.body)
                    .map(cartopolis_core::photo_pose::display_caption)
                    .filter(|c| !c.is_empty()),
            }
        } else if let (Some(point), Some(n)) = (&line.point, n) {
            What::Point(point_v(point, n, &line.body))
        } else {
            What::Text
        };
        items.push(Item::Line(LineV {
            seq: line.seq,
            gap,
            mine,
            who,
            sender: line.sender.clone(),
            log_seq: line.log_seq,
            body: line.body.clone(),
            time: (line.at_ms > 0).then(|| chats::clock_time(line.at_ms)),
            what,
        }));
    }
    ThreadV {
        items,
        empty,
        request,
        extras: Extras {
            community: topic.as_ref().map(|t| t.community_name.clone()),
            moderate_topic: topic
                .as_ref()
                .filter(|t| t.moderates)
                .map(|t| t.name.clone()),
            admin: w
                .identity
                .may(cartopolis_core::permissions::Permission::AccountsManage),
        },
    }
}

/// A point's card as `chats::point_card` words it.
fn point_v(point: &Point, number: usize, caption: &str) -> PointV {
    let title = point
        .name
        .clone()
        .unwrap_or_else(|| format!("{:.4}, {:.4}", point.latitude, point.longitude));
    let text =
        (point.name.is_some() && !caption.is_empty() && point.name.as_deref() != Some(caption))
            .then(|| caption.to_string());
    let coords = format!(
        "{:.4}° {}, {:.4}° {}",
        point.latitude.abs(),
        if point.latitude >= 0.0 { 'N' } else { 'S' },
        point.longitude.abs(),
        if point.longitude >= 0.0 { 'E' } else { 'W' },
    );
    let seen = match point.view {
        Some(v) => format!("{coords} · seen from {:.0} m", v.altitude.max(0.0)),
        None => coords,
    };
    PointV {
        point: point.clone(),
        number,
        title,
        sub: match text {
            Some(t) => format!("{t}\n{seen}"),
            None => seen,
        },
    }
}

/// What a meeting left, as `chats::meeting_card` counts it; `None` for an
/// update that says nothing.
fn meeting_v(
    chat: &ChatState,
    key: &str,
    line: &ChatLine,
    at: usize,
    meeting: &MeetingObject,
    thread: &[ChatLine],
    now: u64,
) -> Option<MeetingV> {
    use crate::systems::calendar::short_day;
    let who = |l: &ChatLine| {
        if chat.is_mine(l) {
            "You".to_string()
        } else {
            l.sender.clone()
        }
    };
    let said: Vec<(String, &MeetingObject)> = thread
        .iter()
        .filter_map(|l| l.meeting.as_ref().map(|m| (who(l), m)))
        .collect();
    match meeting {
        MeetingObject::Plan(first) => {
            let plan = cartopolis_core::meeting::current_plan(
                first,
                &line.sender,
                thread
                    .iter()
                    .filter_map(|l| l.meeting.as_ref().map(|m| (l.sender.as_str(), m))),
            );
            let offset = &crate::systems::calendar::offset_s;
            let date = plan.next_date(now, offset).unwrap_or_else(|| {
                plan.dates(plan.start_ms, now, offset)
                    .last()
                    .copied()
                    .unwrap_or(plan.start_ms)
            });
            let answers = cartopolis_core::meeting::tally_rsvps(
                &plan,
                date,
                said.iter().map(|(w, m)| (w.as_str(), *m)),
            );
            let mine = answers.iter().find(|(w, _)| w == "You").map(|(_, r)| *r);
            let going: Vec<&str> = answers
                .iter()
                .filter(|(_, r)| *r == Rsvp::Going)
                .map(|(w, _)| w.as_str())
                .collect();
            let end = date + plan.duration_ms();
            let live = now + 15 * 60_000 >= date && now <= end;
            let open = now <= end;
            let rule = crate::systems::calendar::rule_words(&plan).map(|rule| {
                let next = if date != plan.start_ms || plan.next_date(now, offset).is_some() {
                    format!(" · next {}", short_day(date))
                } else {
                    String::new()
                };
                format!("{rule}{next}")
            });
            let last = plan.repeat.and_then(|r| r.until_ms) == Some(date);
            let own = (chat.is_mine(line) && plan.repeat.is_some() && open)
                .then(|| (short_day(date), last));
            Some(MeetingV::Plan(PlanV {
                when: format!(
                    "Meeting · {}",
                    crate::systems::meetup::when_label(date, now, chats::local_offset_s(date))
                ),
                hours: format!("{}–{}", chats::clock_time(date), chats::clock_time(end)),
                rule,
                going: (!going.is_empty()).then(|| format!("{} going", going.join(", "))),
                live,
                join: live && crate::systems::private_chat::group_of(key).is_some(),
                answers: open.then(|| {
                    [Rsvp::Going, Rsvp::Maybe, Rsvp::No]
                        .into_iter()
                        .map(|r| (r, answers.iter().filter(|(_, a)| *a == r).count()))
                        .collect()
                }),
                mine,
                own,
                date,
                plan,
            }))
        }
        MeetingObject::Update(after) => {
            let original = thread.iter().find_map(|l| match &l.meeting {
                Some(MeetingObject::Plan(p)) if p.id == after.id => Some((l, p)),
                _ => None,
            });
            let text = match original {
                Some((planned, _)) if planned.sender != line.sender => return None,
                Some((planned, p)) => {
                    let before = cartopolis_core::meeting::current_plan(
                        p,
                        &planned.sender,
                        thread[..at]
                            .iter()
                            .filter_map(|l| l.meeting.as_ref().map(|m| (l.sender.as_str(), m))),
                    );
                    cartopolis_core::meeting::PlanChange::between(&before, after)
                        .words(&after.title, &crate::systems::calendar::long_day)
                }
                None => format!("changed {}", after.title),
            };
            Some(MeetingV::Change(format!("{} {text}", who(line))))
        }
        MeetingObject::Poll(poll) => {
            let counted = cartopolis_core::meeting::tally_votes(
                poll,
                said.iter().map(|(w, m)| (w.as_str(), *m)),
            );
            let mine: Vec<String> = counted
                .iter()
                .filter(|(_, who)| who.iter().any(|w| w == "You"))
                .map(|(o, _)| o.clone())
                .collect();
            let voters = {
                let mut v: Vec<&String> = counted.iter().flat_map(|(_, w)| w).collect();
                v.sort();
                v.dedup();
                v.len()
            };
            Some(MeetingV::Poll(PollV {
                options: counted
                    .iter()
                    .map(|(o, who)| (o.clone(), who.len(), mine.contains(o)))
                    .collect(),
                poll: poll.clone(),
                voters,
                mine,
            }))
        }
        MeetingObject::Note { kind, text } => Some(MeetingV::Note {
            kind: *kind,
            text: format!("{text} · {}", who(line)),
        }),
        MeetingObject::Record(r) => {
            let mins = (r.end_ms.saturating_sub(r.start_ms) / 60_000).max(1);
            let mut sections: Vec<(String, Vec<String>)> = Vec::new();
            for kind in [NoteKind::Decision, NoteKind::Action, NoteKind::Note] {
                let notes: Vec<String> = r
                    .notes
                    .iter()
                    .filter(|n| n.kind == kind)
                    .map(|n| n.text.clone())
                    .collect();
                if !notes.is_empty() {
                    sections.push((format!("{}s", kind.label()), notes));
                }
            }
            for p in &r.polls {
                sections.push((
                    p.question.clone(),
                    p.results.iter().map(|(o, n)| format!("{o}: {n}")).collect(),
                ));
            }
            Some(MeetingV::Record(RecordV {
                title: if r.title.is_empty() {
                    "Meeting record".to_string()
                } else {
                    r.title.clone()
                },
                line: format!(
                    "{}–{} · {mins} min · {}",
                    chats::clock_time(r.start_ms),
                    chats::clock_time(r.end_ms),
                    r.people.join(", ")
                ),
                sections,
                transcript: r
                    .transcript
                    .iter()
                    .map(|(who, text)| format!("{who}: {text}"))
                    .collect(),
                copy: format!("{}\n{}", r.title, cartopolis_core::meeting::record_text(r)),
            }))
        }
        // Counted on what they answer (`is_line`).
        MeetingObject::Answer { .. } | MeetingObject::Vote { .. } => None,
    }
}

/// A meet-up as `chats::meetup_card` words it.
fn meetup_v(
    chat: &ChatState,
    key: &str,
    line: &ChatLine,
    thread: &[ChatLine],
    now: u64,
) -> Option<MeetupV> {
    let point = line.point.as_ref()?;
    let m = point.meetup?;
    let people = chats::meetup_people(chat, key, line, thread);
    let going = people.iter().any(|(n, _)| n == "You");
    let place = point
        .name
        .clone()
        .unwrap_or_else(|| "this spot".to_string());
    let when =
        crate::systems::meetup::when_label(m.start_ms, now, chats::local_offset_s(m.start_ms));
    let by = if chat.is_mine(line) {
        "you suggested it".to_string()
    } else {
        format!("suggested by {}", line.sender)
    };
    let from = crate::systems::meetup::when_label(
        m.start_ms
            .saturating_sub(crate::systems::chat::MEETUP_SHARE_BEFORE_MS),
        now,
        chats::local_offset_s(m.start_ms),
    );
    Some(MeetupV {
        point: point.clone(),
        title: format!("Meet at {place}"),
        place,
        sub: format!("{when} · {by}"),
        people,
        going,
        note: format!(
            "Going shares your real location with this chat from {} until you arrive.",
            from.to_lowercase()
        ),
    })
}

/// Everything above the field, as `chats::chat_screen`'s band and
/// `chats::composer` show it.
fn foot_v(w: &ChatsWorld, key: &str, now: u64) -> FootV {
    let chat: &ChatState = &w.chat;
    let private = kind_of(key) == ChatKind::Private;
    let draft = chat
        .meetup_draft
        .as_ref()
        .filter(|(k, _)| private && k.as_str() == key)
        .and_then(|(_, p)| {
            p.meetup.map(|m| DraftV {
                place: p.name.clone().unwrap_or_else(|| "this spot".to_string()),
                halfway: chat.locations.iter().any(|l| l.channel == key),
                when: crate::systems::meetup::when_label(
                    m.start_ms,
                    now,
                    chats::local_offset_s(m.start_ms),
                ),
            })
        });
    let sharing = chat
        .sharing
        .iter()
        .find(|s| private && s.channel == key)
        .map(|s| s.until_ms.saturating_sub(now) / 60_000 + 1);
    let locations = chat
        .locations
        .iter()
        .filter(|l| private && l.channel == key)
        .map(|l| (l.from.clone(), l.latitude, l.longitude))
        .collect();
    let attach = w.view.attach.then(|| AttachV {
        public: !private,
        entries: if private {
            chats::attach_entries(chat, key)
        } else {
            Vec::new()
        },
        share_open: w.view.share_open,
        sharing: chat.sharing.iter().any(|s| s.channel == key),
    });
    FootV {
        key: key.to_string(),
        sending: w.pictures.sending(key),
        waiting: w.view.capturing_for(key),
        error: w.view.picture_error().map(str::to_string),
        draft,
        sharing,
        locations,
        attach,
    }
}

// ─── The screen ──────────────────────────────────────────────────────────────

/// The open chat's screen, for the chat `key`.
pub(crate) fn screen(live: &ChatsLive, key: &str) -> View {
    let head = {
        let l = live.clone();
        let key = key.to_string();
        move || -> View {
            match l.conversation.head.get() {
                Some(h) => head_view(&l, &key, &h),
                None => header(
                    back(l.on(|_, a| a.back = true), "All chats"),
                    View::Empty,
                    View::Empty,
                ),
            }
        }
    };
    let world = {
        let k = key.to_string();
        live.on(move |_, a| a.world_of = Some(k.clone()))
    };
    let thread = {
        let l = live.clone();
        let key = key.to_string();
        move || -> View {
            let t = l.conversation.thread.get();
            let shown = l.conversation.shown.get();
            thread_view(&l, &key, &t, shown)
        }
    };
    // The thread opens at its latest line and follows the new ones: the
    // renderer action `@scroll-end`, given again each time the thread moves.
    let at_end = {
        let thread = live.conversation.thread.clone();
        move || {
            thread.get();
            true
        }
    };
    let scroller = mithril::view::el("div")
        .attr(
            "style",
            "flex: 1; min-height: 0; overflow-y: auto; padding: 8px 20px 20px; display: flex; flex-direction: column",
        )
        .prop_bool("@scroll-end", at_end)
        .child(thread)
        .into_view();
    view! {
        {head}
        <div style="flex: none; display: flex; padding: 12px 20px 0">
            <ButtonGroup attached=true>
                <Button variant=Variant::Primary size=Size::Sm icon={icon(icons::CHAT)}>"Chat"</Button>
                <Button size=Size::Sm icon={icon(icons::CAM_EARTH)} onclick={world} title="This chat's places on the map">"World"</Button>
            </ButtonGroup>
        </div>
        {scroller}
        {composer(live, key)}
    }
}

/// An icon glyph at a size.
fn glyph(g: &'static str, size: u32) -> View {
    let style = format!(
        "font-family: 'Material Symbols Rounded'; font-size: {size}px; line-height: 1; font-weight: 400"
    );
    view! { <span style={style}>{g}</span> }
}

/// A line of text in a colour.
fn small(text: String, color: &str) -> View {
    let style = format!("color: {color}; font-size: 13px");
    view! { <span style={style}>{text}</span> }
}

/// The header: back, the disc, the name over its status, the call key.
fn head_view(l: &ChatsLive, key: &str, h: &HeadV) -> View {
    let lead = view! {
        {back(l.on(|_, a| a.back = true), "All chats")}
        {disc(&h.name, h.kind, 36)}
    };
    let line = "display: flex; align-items: center; gap: 4px; font-size: var(--m-font-size-sm); font-weight: 400";
    let sub: View = match &h.status {
        Status::Location(text) => {
            let style = format!("{line}; color: var(--m-info)");
            let text = text.clone();
            view! { <span style={style}>{glyph(icons::HERE, 14)}<span>{text}</span></span> }
        }
        Status::Topic(text, id) => {
            let id = *id;
            let open = l.tap(move |_, a| a.show_community = Some(id));
            let style = format!(
                "{line}; margin: 0; padding: 0; border: 0; background: none; color: var(--m-text-dim); font-family: inherit; cursor: pointer"
            );
            let text = text.clone();
            view! {
                <button title="Open the community" style={style} on:click={open}>
                    <span>{text}</span>{glyph(icons::CHEVRON_RIGHT, 14)}
                </button>
            }
        }
        Status::Plain(kind) => {
            // `subtitle` leads with the padlock glyph, which needs the icon
            // font here.
            let words = chats::subtitle(*kind);
            let (lock, words) = match words.strip_prefix(icons::LOCK) {
                Some(rest) => (glyph(icons::LOCK, 14), rest.trim_start().to_string()),
                None => (View::Empty, words.clone()),
            };
            let style = format!("{line}; color: var(--m-text-dim)");
            view! { <span style={style}>{lock}<span>{words}</span></span> }
        }
    };
    let name = h.name.clone();
    let title = view! {
        <div style="display: flex; flex-direction: column; gap: 2px; min-width: 0">
            <span style="font-size: 18px; font-weight: 600; color: var(--m-text-strong); white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{name}</span>
            {sub}
        </div>
    };
    header(lead, title, call_view(l, key, h.call))
}

/// The call key: call or join (with the word, so it does not read as
/// "start one"), or hang up.
fn call_view(l: &ChatsLive, key: &str, call: CallButton) -> View {
    let (g, title, variant, hang) = match call {
        CallButton::Hidden => return View::Empty,
        CallButton::Call => (icons::CALL, "Call", Variant::Primary, false),
        CallButton::Join => (icons::CALL, "Join the call", Variant::Primary, false),
        CallButton::HangUp => (icons::CALL_END, "Hang up", Variant::Danger, true),
    };
    let k = key.to_string();
    let tap = l.on(move |_, a| {
        if hang {
            a.call = Some(CallRequest::Leave);
        } else if let Some(group) = crate::systems::private_chat::group_of(&k) {
            a.call = Some(CallRequest::Join(group));
        }
    });
    let join: View = if call == CallButton::Join {
        view! { <span style="color: var(--m-success); font-size: var(--m-font-size-sm); font-weight: 600">"Join"</span> }
    } else {
        View::Empty
    };
    view! {
        {join}
        <Button variant={variant} icon={icon(g)} icon_only=true onclick={tap} title={title} />
    }
}

// ─── The thread ──────────────────────────────────────────────────────────────

fn thread_view(l: &ChatsLive, key: &str, t: &ThreadV, shown: usize) -> View {
    let request: View = match &t.request {
        Some(r) => note(r.clone()),
        None => View::Empty,
    };
    let empty: View = match t.empty {
        Some(line) => view! {
            <div style="display: flex; flex-direction: column; align-items: center; gap: 6px; margin: auto 0; padding: 24px; text-align: center">
                <span style="color: var(--m-text-strong); font-size: 17px; font-weight: 600">"Nothing here yet"</span>
                <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{line}</span>
            </div>
        },
        None => View::Empty,
    };
    let from = t.items.len().saturating_sub(shown);
    let earlier: View = if from > 0 {
        let s = l.conversation.shown.clone();
        let more: Handler = Rc::new(move |_: &Event| s.set(s.get_untracked() + PAGE));
        view! {
            <div style="display: flex; justify-content: center">
                <Button variant=Variant::Ghost size=Size::Sm onclick={Some(more)}>"Show earlier messages"</Button>
            </div>
        }
    } else {
        View::Empty
    };
    let items: Vec<View> = t.items[from..]
        .iter()
        .map(|i| item_view(l, key, t, i))
        .collect();
    view! { {request} {empty} {earlier} {items} }
}

fn item_view(l: &ChatsLive, key: &str, t: &ThreadV, item: &Item) -> View {
    match item {
        Item::Shared(text) => centred(text.clone(), "var(--m-text-dim)", 14),
        Item::Removed => centred(
            "Message removed by a moderator".to_string(),
            "var(--m-text-faint)",
            8,
        ),
        Item::Day(day) => {
            let day = day.clone();
            view! {
                <div style="display: flex; justify-content: center; margin-top: 16px">
                    <span style="padding: 3px 12px; border-radius: var(--m-radius-full); background: var(--m-surface-raised); color: var(--m-text-dim); font-size: 12px; font-weight: 600">{day}</span>
                </div>
            }
        }
        Item::Going(words) => {
            let words = words.clone();
            view! {
                <div style="display: flex; justify-content: center; align-items: center; gap: 6px; margin-top: 8px; color: var(--m-info); font-size: 12px">
                    {glyph(icons::CHECK, 14)}<span>{words}</span>
                </div>
            }
        }
        Item::Line(line) => line_view(l, key, &t.extras, line),
        Item::Meeting(m) => meeting_view(l, key, m),
        Item::Meetup(m) => meetup_view(l, key, m),
    }
}

/// A centred quiet line, `gap` px below what is above it.
fn centred(text: String, color: &str, gap: u32) -> View {
    let style = format!(
        "margin: {gap}px 0 0; text-align: center; color: {color}; font-size: var(--m-font-size-sm)"
    );
    view! { <p style={style}>{text}</p> }
}

/// A line: the speaker's name (which opens their menu) over a bubble, a
/// picture or a point, on the speaker's side.
fn line_view(l: &ChatsLive, key: &str, x: &Extras, line: &LineV) -> View {
    let align = if line.mine { "flex-end" } else { "flex-start" };
    let wrap = format!(
        "display: flex; flex-direction: column; align-items: {align}; gap: 6px; margin-top: {}px",
        line.gap
    );
    let name: View = match &line.who {
        Some(who) => {
            let (menu, armed, seq) = (
                l.conversation.menu.clone(),
                l.conversation.armed.clone(),
                line.seq,
            );
            let toggle = move |_: &Event| {
                let open = menu.get_untracked() == Some(seq);
                menu.set((!open).then_some(seq));
                armed.set(None);
            };
            let who = who.clone();
            view! {
                <button title="Message, block or report" style="margin: 0; padding: 0 4px; border: 0; background: none; color: var(--m-accent-text); font-family: inherit; font-size: var(--m-font-size-sm); font-weight: 650; cursor: pointer" on:click={toggle}>{who}</button>
            }
        }
        None => View::Empty,
    };
    let body = match &line.what {
        What::Text => text_bubble(line),
        What::Picture { picture, caption } => picture_view(l, line, picture, caption),
        What::Point(p) => point_view(l, line, p),
    };
    let menu: View = if line.who.is_some() {
        let l = l.clone();
        let (k, x, line) = (key.to_string(), x.clone(), line.clone());
        (move || -> View {
            if l.conversation.menu.get() == Some(line.seq) {
                person_menu(&l, &k, &x, &line)
            } else {
                View::Empty
            }
        })
        .into_view()
    } else {
        View::Empty
    };
    view! { <div style={wrap}>{name}{body}{menu}</div> }
}

fn time_view(time: &Option<String>) -> View {
    match time {
        Some(t) => {
            let t = t.clone();
            view! { <span style="align-self: flex-end; color: var(--m-text-dim); font-size: 11px">{t}</span> }
        }
        None => View::Empty,
    }
}

/// A bubble: yours in the accent's soft tint, theirs raised, the corner on
/// the speaker's side tucked in.
fn text_bubble(line: &LineV) -> View {
    let (bg, ink, radius) = if line.mine {
        (
            "var(--m-accent-soft)",
            "var(--m-text-strong)",
            "18px 18px 6px 18px",
        )
    } else {
        (
            "var(--m-surface-raised)",
            "var(--m-text)",
            "18px 18px 18px 6px",
        )
    };
    let style = format!(
        "display: flex; flex-direction: column; gap: 4px; max-width: 80%; padding: 10px 14px; border-radius: {radius}; background: {bg}; color: {ink}"
    );
    let body = line.body.clone();
    view! {
        <div style={style}>
            <span style="font-size: 15px">{body}</span>
            {time_view(&line.time)}
        </div>
    }
}

/// A picture: the library has no image node, so a tile of the picture's
/// own shape naming it, which opens egui's full-size viewer; its caption on
/// the card's deep bottom edge, as the Polaroid egui draws.
fn picture_view(
    l: &ChatsLive,
    line: &LineV,
    picture: &PictureRef,
    caption: &Option<String>,
) -> View {
    let size = crate::systems::chat_pictures::fit(picture, PICTURE_MAX, PICTURE_MAX);
    let p = picture.clone();
    let open = l.tap(move |_, a| a.view_picture = Some(p.clone()));
    let tile = format!(
        "display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px; width: {:.0}px; height: {:.0}px; margin: 0; padding: 0; border: 0; border-radius: 4px; background: var(--m-surface-sunken); color: var(--m-text-dim); font-family: inherit; cursor: pointer",
        size.x.max(140.0),
        size.y.max(100.0)
    );
    let label = format!("A picture · {} × {}", picture.width, picture.height);
    let caption: View = match caption {
        Some(c) => {
            let c = c.clone();
            view! { <span style="color: var(--m-text-strong); font-size: 14px">{c}</span> }
        }
        None => View::Empty,
    };
    view! {
        <div style="display: flex; flex-direction: column; gap: 8px; padding: 10px 10px 8px; border: 1px solid var(--m-border); border-radius: 6px; background: var(--m-surface); box-shadow: var(--m-shadow)">
            <button title="Open full size" style={tile} on:click={open}>
                {glyph(icons::CAMERA, 32)}
                <span style="font-size: var(--m-font-size-sm)">{label}</span>
            </button>
            {caption}
            {time_view(&line.time)}
        </div>
    }
}

/// A point: its number, where its picture goes, its name and line, Show in
/// world and Route.
fn point_view(l: &ChatsLive, line: &LineV, p: &PointV) -> View {
    let radius = if line.mine {
        "18px 18px 6px 18px"
    } else {
        "18px 18px 18px 6px"
    };
    let card = format!(
        "display: flex; flex-direction: column; width: 100%; max-width: 280px; border: 1px solid var(--m-border); border-radius: {radius}; background: var(--m-surface-raised); overflow: hidden"
    );
    let point = p.point.clone();
    let go = l.on(move |_, a| a.go_to = Some(point.clone()));
    let (lat, lng) = (p.point.latitude, p.point.longitude);
    let name = p.point.name.clone().unwrap_or_else(|| line.body.clone());
    let route = l.on(move |_, a| a.route_to = Some((lat, lng, name.clone())));
    let (title, sub, number) = (p.title.clone(), p.sub.clone(), p.number.to_string());
    view! {
        <div style={card}>
            <div style="position: relative; display: flex; align-items: center; justify-content: center; height: 112px; background: var(--m-surface-sunken); color: var(--m-accent)">
                {glyph(icons::PIN, 36)}
                <span style="position: absolute; left: 10px; top: 10px; display: flex; align-items: center; justify-content: center; width: 24px; height: 24px; border-radius: var(--m-radius-full); background: var(--m-text-strong); color: var(--m-bg); font-size: 13px; font-weight: 700">{number}</span>
            </div>
            <div style="display: flex; flex-direction: column; gap: 4px; padding: 12px 14px 14px">
                <span style="color: var(--m-text-strong); font-size: 15px; font-weight: 600">{title}</span>
                <span style="color: var(--m-text-dim); font-size: 12px">{sub}</span>
                <div style="display: flex; gap: 8px; margin-top: 8px">
                    <Button variant=Variant::Primary size=Size::Sm icon={icon(icons::CAM_EARTH)} onclick={go} title="Show it in the world, from where it was sent">"Show in world"</Button>
                    <Button size=Size::Sm icon={icon(icons::ROUTE)} onclick={route} title="Plan a route there">"Route"</Button>
                </div>
            </div>
        </div>
    }
}

/// The menu behind a speaker's name (`chat::person_menu`): message, block,
/// report; a moderator's and a server admin's acts when they are those.
fn person_menu(l: &ChatsLive, key: &str, x: &Extras, line: &LineV) -> View {
    let entry = |pick: Pick, g: &'static str, label: String, danger: bool| -> View {
        let go = {
            let (k, sender, body, seq) = (
                key.to_string(),
                line.sender.clone(),
                line.body.clone(),
                line.log_seq,
            );
            let (menu, armed) = (l.conversation.menu.clone(), l.conversation.armed.clone());
            l.later(move |w, a| {
                let topic = TopicContext::of(&w.communities, &k);
                chats::person_act(pick.action(&body), &sender, seq, topic.as_ref(), a);
                menu.set(None);
                armed.set(None);
            })
        };
        let tap: Handler = {
            let armed = l.conversation.armed.clone();
            Rc::new(move |_: &Event| {
                if pick.confirm().is_none() || armed.get_untracked() == Some(pick) {
                    go();
                } else {
                    armed.set(Some(pick));
                }
            })
        };
        let words = {
            let armed = l.conversation.armed.clone();
            move || -> String {
                match pick.confirm() {
                    Some(ask) if armed.get() == Some(pick) => ask.to_string(),
                    _ => label.clone(),
                }
            }
        };
        let tone = danger.then_some(Tone::Danger);
        view! { <ListItem icon={icon(g)} tone={tone} onclick={Some(tap)}>{words}</ListItem> }
    };
    let mut rows = vec![
        entry(
            Pick::Message,
            icons::CHAT,
            "Message privately".into(),
            false,
        ),
        entry(Pick::Block, icons::STOP, "Block".into(), true),
    ];
    if x.community.is_some() {
        rows.push(entry(
            Pick::ReportMods,
            icons::FLAG,
            "Report to the moderators…".into(),
            false,
        ));
    }
    rows.push(entry(
        Pick::Report,
        icons::FLAG,
        "Report to the server…".into(),
        false,
    ));
    let moderator: View = match (&x.moderate_topic, &x.community) {
        (Some(topic), Some(community)) => view! {
            {group(format!("Moderator · {community}"))}
            <List>
                {entry(Pick::DeleteAll, icons::DELETE, "Delete for everyone".into(), true)}
                {entry(Pick::RemoveTopic, icons::PERSON_REMOVE, format!("Remove from {topic}"), true)}
                {entry(Pick::RemoveCommunity, icons::PERSON_REMOVE, "Remove from the community".into(), true)}
            </List>
        },
        _ => View::Empty,
    };
    let admin: View = if x.admin {
        view! {
            {group("On this server · you are an admin")}
            <List>
                {entry(Pick::Suspend, icons::STOP, "Suspend account".into(), true)}
                {entry(Pick::DeleteAccount, icons::DELETE, "Delete account…".into(), true)}
            </List>
        }
    } else {
        View::Empty
    };
    let who = line.sender.clone();
    view! {
        <div style="display: flex; flex-direction: column; gap: 8px; width: 100%; max-width: 300px; padding: 12px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface); box-shadow: var(--m-shadow-lg)">
            <span style="padding-inline: 4px; color: var(--m-text-strong); font-weight: 600">{who}</span>
            <List>{rows}</List>
            {moderator}
            {admin}
        </div>
    }
}

// ─── The cards ───────────────────────────────────────────────────────────────

/// A card in the thread: raised, the accent's edge while it is live.
fn card_style(live: bool) -> String {
    let edge = if live {
        "2px solid var(--m-accent)"
    } else {
        "1px solid var(--m-border)"
    };
    format!(
        "display: flex; flex-direction: column; gap: 6px; width: 100%; max-width: 340px; margin-top: 14px; padding: 14px 16px; border: {edge}; border-radius: 18px; background: var(--m-surface-raised)"
    )
}

/// A card's small capitals over its title.
fn kicker(g: &'static str, text: &str) -> View {
    let text = text.to_uppercase();
    view! {
        <span style="display: flex; align-items: center; gap: 6px; color: var(--m-text-dim); font-size: 11px; font-weight: 700; letter-spacing: 0.06em">
            {glyph(g, 14)}<span>{text}</span>
        </span>
    }
}

fn card_title(text: String) -> View {
    view! { <span style="color: var(--m-text-strong); font-size: 16px; font-weight: 600">{text}</span> }
}

/// What a meeting left: a plan with its answers, an update's line, a poll
/// with its votes, a note, a record.
fn meeting_view(l: &ChatsLive, key: &str, m: &MeetingV) -> View {
    let dim = "var(--m-text-dim)";
    let ink = "var(--m-text)";
    match m {
        MeetingV::Plan(p) => {
            let mut parts: Vec<View> = vec![
                kicker(icons::EVENT, &p.when),
                card_title(p.plan.title.clone()),
                small(p.hours.clone(), dim),
            ];
            if let Some(rule) = &p.rule {
                parts.push(small(rule.clone(), "var(--m-accent-text)"));
            }
            if let Some(text) = &p.plan.text {
                parts.push(small(text.clone(), ink));
            }
            for (i, item) in p.plan.agenda.iter().enumerate() {
                let minutes = if item.minutes > 0 {
                    format!("  · {} min", item.minutes)
                } else {
                    String::new()
                };
                parts.push(small(format!("{}. {}{minutes}", i + 1, item.name), ink));
            }
            if let Some(going) = &p.going {
                parts.push(small(going.clone(), dim));
            }
            if p.join {
                let k = key.to_string();
                let join = l.on(move |_, a| {
                    if let Some(group) = crate::systems::private_chat::group_of(&k) {
                        a.call = Some(CallRequest::Join(group));
                    }
                });
                parts.push(view! {
                    <Button variant=Variant::Primary icon={icon(icons::VIDEOCAM)} onclick={join}>"Join"</Button>
                });
            }
            if let Some(answers) = &p.answers {
                let chips: Vec<View> = answers
                    .iter()
                    .map(|&(r, n)| {
                        let (k, plan, date) = (key.to_string(), p.plan.clone(), p.date);
                        let tap = l.on(move |_, a| {
                            a.meeting = Some((k.clone(), chats::rsvp_answer(&plan, date, r)));
                        });
                        let label = if n > 0 {
                            format!("{} {n}", r.words())
                        } else {
                            r.words().to_string()
                        };
                        let variant = if p.mine == Some(r) {
                            Variant::Primary
                        } else {
                            Variant::Secondary
                        };
                        view! { <Button variant={variant} size=Size::Sm onclick={tap}>{label}</Button> }
                    })
                    .collect();
                parts.push(view! {
                    <div style="display: flex; flex-wrap: wrap; gap: 8px; margin-top: 4px">{chips}</div>
                });
            }
            if let Some((day, last)) = &p.own {
                let date = p.date;
                let skip = {
                    let (k, plan) = (key.to_string(), p.plan.clone());
                    l.on(move |_, a| a.meeting = Some((k.clone(), chats::skip_date(&plan, date))))
                };
                let stop: View = if *last {
                    View::Empty
                } else {
                    let (k, plan) = (key.to_string(), p.plan.clone());
                    let stop = l.on(move |_, a| {
                        a.meeting = Some((k.clone(), chats::stop_after(&plan, date)));
                    });
                    let label = format!("Stop after {day}");
                    view! { <Button size=Size::Sm onclick={stop}>{label}</Button> }
                };
                let label = format!("Skip {day}");
                parts.push(view! {
                    <div style="display: flex; flex-wrap: wrap; gap: 8px">
                        <Button size=Size::Sm onclick={skip}>{label}</Button>
                        {stop}
                    </div>
                });
            }
            let style = card_style(p.live);
            view! { <div style={style}>{parts}</div> }
        }
        MeetingV::Change(text) => {
            let text = text.clone();
            view! {
                <div style="display: flex; justify-content: center; align-items: center; gap: 6px; margin-top: 10px; color: var(--m-text-dim); font-size: 12px">
                    {glyph(icons::EVENT, 14)}<span>{text}</span>
                </div>
            }
        }
        MeetingV::Poll(p) => {
            let options: Vec<View> = p
                .options
                .iter()
                .map(|(option, n, picked)| {
                    let (k, poll, mine, opt) =
                        (key.to_string(), p.poll.clone(), p.mine.clone(), option.clone());
                    let tap = l.tap(move |_, a| {
                        a.meeting = Some((k.clone(), chats::poll_vote(&poll, &mine, &opt)));
                    });
                    let share = if p.voters > 0 {
                        *n as f32 / p.voters as f32 * 100.0
                    } else {
                        0.0
                    };
                    let edge = if *picked {
                        "var(--m-accent)"
                    } else {
                        "var(--m-border)"
                    };
                    let row = format!(
                        "position: relative; display: flex; align-items: center; gap: 8px; width: 100%; min-height: 40px; margin: 0; padding: 0 12px; border: 1px solid {edge}; border-radius: 10px; background: var(--m-surface); color: var(--m-text); font-family: inherit; text-align: start; cursor: pointer; overflow: hidden"
                    );
                    let bar = format!(
                        "position: absolute; left: 0; top: 0; bottom: 0; width: {share:.0}%; background: var(--m-accent-soft)"
                    );
                    let mark: View = if *picked {
                        glyph(icons::CHECK, 16)
                    } else {
                        View::Empty
                    };
                    let (title, label, count) = (option.clone(), option.clone(), n.to_string());
                    view! {
                        <button title={title} style={row} on:click={tap}>
                            <span style={bar}></span>
                            <span style="position: relative; flex: 1; display: flex; align-items: center; gap: 6px">{mark}<span>{label}</span></span>
                            <span style="position: relative; color: var(--m-text-dim); font-size: 13px">{count}</span>
                        </button>
                    }
                })
                .collect();
            let kick = if p.poll.multiple {
                "Poll · choose any"
            } else {
                "Poll · choose one"
            };
            let footer = format!(
                "{} {} · everybody in the chat sees who chose what",
                p.voters,
                if p.voters == 1 { "vote" } else { "votes" }
            );
            let style = card_style(false);
            view! {
                <div style={style}>
                    {kicker(icons::POLL, kick)}
                    {card_title(p.poll.question.clone())}
                    <div style="display: flex; flex-direction: column; gap: 6px; margin-top: 4px">{options}</div>
                    {small(footer, dim)}
                </div>
            }
        }
        MeetingV::Note { kind, text } => {
            let tint = match kind {
                NoteKind::Decision => "var(--m-accent-text)",
                NoteKind::Action => "var(--m-warning)",
                NoteKind::Note => dim,
            };
            let style =
                format!("color: {tint}; font-size: 11px; font-weight: 700; letter-spacing: 0.06em");
            let (label, text) = (kind.label().to_uppercase(), text.clone());
            view! {
                <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 8px; max-width: 340px; margin-top: 10px">
                    <span style={style}>{label}</span>
                    <span style="color: var(--m-text); font-size: 14px">{text}</span>
                </div>
            }
        }
        MeetingV::Record(r) => {
            let mut parts: Vec<View> = vec![
                kicker(icons::RECORD, "Meeting record"),
                card_title(r.title.clone()),
                small(r.line.clone(), dim),
            ];
            for (head, rows) in &r.sections {
                let head = head.clone();
                parts.push(view! {
                    <span style="margin-top: 4px; color: var(--m-text-strong); font-size: 13px; font-weight: 600">{head}</span>
                });
                for row in rows {
                    parts.push(small(format!("•  {row}"), ink));
                }
            }
            if !r.transcript.is_empty() {
                let open = signal(false);
                let toggle: Handler = {
                    let open = open.clone();
                    Rc::new(move |_: &Event| open.set(!open.get_untracked()))
                };
                let label = format!("Transcript · {} lines", r.transcript.len());
                let lines = r.transcript.clone();
                let body = move || -> View {
                    if open.get() {
                        lines
                            .iter()
                            .map(|t| small(t.clone(), "var(--m-text)"))
                            .collect::<Vec<View>>()
                            .into_view()
                    } else {
                        View::Empty
                    }
                };
                parts.push(view! {
                    <div style="display: flex">
                        <Button variant=Variant::Ghost size=Size::Sm onclick={Some(toggle)}>{label}</Button>
                    </div>
                    {body}
                });
            }
            let copy: Handler = {
                let (cell, text) = (l.conversation.copy.clone(), r.copy.clone());
                Rc::new(move |_: &Event| *cell.borrow_mut() = Some(text.clone()))
            };
            parts.push(view! {
                <div style="display: flex; margin-top: 4px">
                    <Button size=Size::Sm icon={icon(icons::COPY)} onclick={Some(copy)}>"Copy as text"</Button>
                </div>
            });
            let style = card_style(false);
            view! { <div style={style}>{parts}</div> }
        }
    }
}

/// A meet-up: where and when, who is going and how far off, what going
/// shares, and I'm going (or Show in world) and Route.
fn meetup_view(l: &ChatsLive, key: &str, m: &MeetupV) -> View {
    let dim = "var(--m-text-dim)";
    let title = m.title.clone();
    let mut parts: Vec<View> = vec![
        view! {
            <span style="display: flex; align-items: center; gap: 8px; color: var(--m-text-strong); font-size: 16px; font-weight: 600">
                {glyph(icons::FLAG, 18)}<span>{title}</span>
            </span>
        },
        small(m.sub.clone(), dim),
    ];
    for (name, status) in &m.people {
        let color = if status == "going" {
            dim
        } else {
            "var(--m-info)"
        };
        let name = name.clone();
        parts.push(view! {
            <div style="display: flex; align-items: center; gap: 8px">
                <span style="flex: 1; color: var(--m-text); font-size: 14px">{name}</span>
                {small(status.clone(), color)}
            </div>
        });
    }
    if !m.going {
        let text = m.note.clone();
        parts.push(view! {
            <div style="padding: 8px 10px; border: 1px solid var(--m-info); border-radius: 12px; background: var(--m-info-soft); color: var(--m-text); font-size: 13px">{text}</div>
        });
    }
    let point = m.point.clone();
    let first: View = if m.going {
        let go = l.on(move |_, a| a.go_to = Some(point.clone()));
        view! { <Button variant=Variant::Primary icon={icon(icons::CAM_EARTH)} onclick={go}>"Show in world"</Button> }
    } else {
        let k = key.to_string();
        let going = l.on(move |_, a| a.meetup_going = Some((k.clone(), point.clone())));
        view! { <Button variant=Variant::Primary icon={icon(icons::CHECK)} onclick={going}>"I'm going"</Button> }
    };
    let (lat, lng, place) = (m.point.latitude, m.point.longitude, m.place.clone());
    let route = l.on(move |_, a| a.route_to = Some((lat, lng, place.clone())));
    parts.push(view! {
        <div style="display: flex; flex-wrap: wrap; gap: 8px; margin-top: 4px">
            {first}
            <Button icon={icon(icons::ROUTE)} onclick={route}>"Route"</Button>
        </div>
    });
    let style = card_style(false);
    view! { <div style={style}>{parts}</div> }
}

// ─── The composer ────────────────────────────────────────────────────────────

/// The band under the thread: what is above the field, and the field (or
/// the form in its place). Built once per chat; its parts follow their own
/// signals, so the field is never rebuilt under the fingers.
fn composer(l: &ChatsLive, key: &str) -> View {
    let above = {
        let l = l.clone();
        move || -> View { foot_view(&l, &l.conversation.foot.get()) }
    };
    let input = {
        let l = l.clone();
        let key = key.to_string();
        move || -> View {
            match l.conversation.form_kind.get() {
                Some(kind) => form_view(&l, &key, kind),
                None => compose_row(&l, &key),
            }
        }
    };
    let frame = {
        let lift = l.conversation.lift.clone();
        move || {
            format!(
                "flex: none; max-height: 75%; overflow-y: auto; display: flex; flex-direction: column; gap: 12px; padding: 12px 20px {:.0}px; border-top: 1px solid var(--m-border); background: var(--m-bg)",
                16.0 + lift.get()
            )
        }
    };
    view! {
        <div style={frame}>
            {above}
            {input}
        </div>
    }
}

fn foot_view(l: &ChatsLive, f: &FootV) -> View {
    let mut parts: Vec<View> = Vec::new();
    if f.sending > 0 {
        let text = if f.sending == 1 {
            "Sending a picture…".to_string()
        } else {
            format!("Sending {} pictures…", f.sending)
        };
        parts.push(view! {
            <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                <Spinner size=Size::Sm />
                <span>{text}</span>
            </div>
        });
    }
    if f.waiting {
        parts.push(note("Waiting for a picture…"));
    }
    if let Some(why) = &f.error {
        let text = format!("Could not get a picture: {why}");
        parts.push(view! {
            <p style="margin: 0; color: var(--m-warning); font-size: var(--m-font-size-sm)">{text}</p>
        });
    }
    if let Some(d) = &f.draft {
        parts.push(draft_view(l, d));
    }
    let info = "display: flex; align-items: center; gap: 6px; color: var(--m-info); font-size: var(--m-font-size-sm)";
    if let Some(left) = f.sharing {
        let k = f.key.clone();
        let stop = l.on(move |_, a| a.stop_share = Some(k.clone()));
        let text = format!("You share your real location here · {left} min left");
        let style = format!("flex: 1; {info}");
        parts.push(view! {
            <div style="display: flex; align-items: center; gap: 8px">
                <span style={style}>{glyph(icons::HERE, 16)}<span>{text}</span></span>
                <Button size=Size::Sm onclick={stop}>"Stop"</Button>
            </div>
        });
    }
    for (from, lat, lng) in &f.locations {
        let (lat, lng) = (*lat, *lng);
        let show = {
            let who = from.clone();
            l.on(move |_, a| a.show_location = Some((lat, lng, who.clone())))
        };
        let route = {
            let (k, who) = (f.key.clone(), from.clone());
            l.on(move |_, a| a.route_to_person = Some((k.clone(), who.clone(), lat, lng)))
        };
        let text = format!("{from} shares their location");
        parts.push(view! {
            <div style="display: flex; flex-direction: column; gap: 8px">
                <span style={info}>{glyph(icons::HERE, 16)}<span>{text}</span></span>
                <div style="display: flex; flex-wrap: wrap; gap: 8px">
                    <Button size=Size::Sm icon={icon(icons::CAM_EARTH)} onclick={show}>"Show in world"</Button>
                    <Button size=Size::Sm icon={icon(icons::ROUTE)} onclick={route} title="Plan a route to them; it follows them as they move">"Route"</Button>
                </div>
            </div>
        });
    }
    if let Some(a) = &f.attach {
        parts.push(attach_view(l, &f.key, a));
    }
    parts.into_view()
}

/// The meet-up being proposed: where, when (a quarter hour at a time), and
/// Send.
fn draft_view(l: &ChatsLive, d: &DraftV) -> View {
    let earlier = l.on(|_, a| a.meetup_shift = Some(-15));
    let later = l.on(|_, a| a.meetup_shift = Some(15));
    let cancel = l.on(|_, a| a.meetup_cancel = true);
    let send = l.on(|_, a| a.meetup_send = true);
    let (title, when) = (format!("Meet at {}", d.place), d.when.clone());
    let hint = if d.halfway {
        "Halfway along the walk between you."
    } else {
        "Where the map is now. Move the map and ask again for somewhere else."
    };
    view! {
        <div style={PANEL}>
            <span style="display: flex; align-items: center; gap: 8px; color: var(--m-text-strong); font-size: 16px; font-weight: 600">
                {glyph(icons::FLAG, 18)}<span>{title}</span>
            </span>
            {note(hint)}
            <div style="display: flex; align-items: center; gap: 8px">
                <Button size=Size::Sm onclick={earlier}>"−15 min"</Button>
                <span style="flex: 1; text-align: center; color: var(--m-text-strong); font-weight: 600">{when}</span>
                <Button size=Size::Sm onclick={later}>"+15 min"</Button>
            </div>
            <div style="display: flex; justify-content: flex-end; gap: 8px">
                <Button onclick={cancel}>"Cancel"</Button>
                <Button variant=Variant::Primary icon={icon(icons::SEND)} onclick={send}>"Send"</Button>
            </div>
        </div>
    }
}

/// The "+" menu: a round key and its word for each entry, and Location's
/// durations once it is open.
fn attach_view(l: &ChatsLive, key: &str, a: &AttachV) -> View {
    if a.public {
        return note("Places and locations go in private chats only: this one is public.");
    }
    let cells: Vec<View> = a
        .entries
        .iter()
        .map(|&e| {
            let k = key.to_string();
            let tap = l.on(move |w, act| chats::pick_attach(e, &mut w.view, &k, unix_ms(), act));
            let variant = if e == Attach::Location && a.share_open {
                Variant::Primary
            } else {
                Variant::Secondary
            };
            view! {
                <div style="display: flex; flex-direction: column; align-items: center; gap: 6px; width: 25%; padding-block: 4px">
                    <Button variant={variant} icon={icon(e.icon())} icon_only=true onclick={tap} title={e.hint()} />
                    <span style="color: var(--m-text); font-size: 12px">{e.label()}</span>
                </div>
            }
        })
        .collect();
    let share: View = if a.share_open && !a.sharing {
        let buttons: Vec<View> = chats::SHARE_FOR
            .iter()
            .map(|&(label, ms)| {
                let k = key.to_string();
                let tap = l.on(move |w, act| chats::share_for(&mut w.view, &k, ms, act));
                view! { <Button size=Size::Sm onclick={tap}>{label}</Button> }
            })
            .collect();
        view! {
            <div style="display: flex; align-items: center; flex-wrap: wrap; gap: 8px">
                <span style="color: var(--m-text-dim)">"for"</span>
                {buttons}
            </div>
            {note(chats::SHARE_NOTE)}
        }
    } else {
        View::Empty
    };
    view! {
        <div style={PANEL}>
            <div style="display: flex; flex-wrap: wrap; row-gap: 8px">{cells}</div>
            {share}
        </div>
    }
}

/// "+", the field (Enter sends; the pin sends this view in a private chat)
/// and send.
fn compose_row(l: &ChatsLive, key: &str) -> View {
    let c = &l.conversation;
    let plus = {
        let l = l.clone();
        move || -> View {
            let g = if l.conversation.attach_open.get() {
                icons::CLEAR
            } else {
                icons::ADD
            };
            let toggle = l.on(|w, _| w.view.attach = !w.view.attach);
            view! {
                <Button icon={icon(g)} icon_only=true onclick={toggle} title="Send a place, or share where you are" />
            }
        }
    };
    let send_now = {
        let (c, k) = (c.clone(), key.to_string());
        l.later(move |w, a| {
            pull(&c, w, &k);
            if !w.view.draft.trim().is_empty() {
                a.send = Some(k.clone());
            }
        })
    };
    let keys: Handler = {
        let go = send_now.clone();
        Rc::new(move |e: &Event| {
            if e.key.as_deref() == Some("Enter") {
                go();
            }
        })
    };
    let send: Handler = Rc::new(move |_: &Event| send_now());
    let pin: View = if ChatState::is_private(key) {
        let k = key.to_string();
        let tap = l.on(move |_, a| a.view_to = Some(k.clone()));
        view! {
            <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::PIN)} icon_only=true onclick={tap} title="Send this view" />
        }
    } else {
        View::Empty
    };
    let send_button = {
        let ready = c.ready.clone();
        move || -> View {
            let off = !ready.get();
            view! {
                <Button variant=Variant::Primary icon={icon(icons::SEND)} icon_only=true disabled={off} onclick={Some(send.clone())} title="Send" />
            }
        }
    };
    let model = c.draft.model.clone();
    view! {
        <div style="display: flex; align-items: center; gap: 10px">
            {plus}
            <div style="flex: 1; min-width: 0">
                <FieldText model={model} placeholder="Message" autocomplete="off" onkeydown={Some(keys)} suffix={pin} id="chats-message-field" />
            </div>
            {send_button}
        </div>
    }
}

/// `+ → Meeting` and `+ → Poll`, in the field's place.
fn form_view(l: &ChatsLive, key: &str, kind: FormKind) -> View {
    let c = &l.conversation;
    let cancel = l.on(|w, _| w.view.meeting_form = None);
    let post = {
        let (c, k) = (c.clone(), key.to_string());
        l.on(move |w, a| {
            pull(&c, w, &k);
            let posted = w
                .view
                .meeting_form
                .as_ref()
                .filter(|(fk, _)| *fk == k)
                .and_then(|(fk, f)| f.object().map(|o| (fk.clone(), o)));
            if let Some(said) = posted {
                a.meeting = Some(said);
                w.view.meeting_form = None;
            }
        })
    };
    let (g, title, hint, word) = match kind {
        FormKind::Plan => (
            icons::EVENT,
            "Plan a meeting",
            "A call in this chat at a time. Everybody here can say whether they come.",
            "Post the plan",
        ),
        FormKind::Poll => (
            icons::POLL,
            "Ask a poll",
            "Votes are messages in this chat: everybody in it sees who chose what.",
            "Ask",
        ),
    };
    let fields: View = match kind {
        FormKind::Plan => plan_fields(l),
        FormKind::Poll => poll_fields(l),
    };
    let post_button = {
        let form = c.form.clone();
        move || -> View {
            let off = !form.get().ready;
            view! {
                <Button variant=Variant::Primary icon={icon(icons::SEND)} disabled={off} onclick={post.clone()}>{word}</Button>
            }
        }
    };
    view! {
        <div style={PANEL}>
            <span style="display: flex; align-items: center; gap: 8px; color: var(--m-text-strong); font-size: 16px; font-weight: 600">
                {glyph(g, 18)}<span>{title}</span>
            </span>
            {note(hint)}
            {fields}
            <div style="display: flex; align-items: center; gap: 8px">
                <Button onclick={cancel}>"Cancel"</Button>
                <div style="flex: 1"></div>
                {post_button}
            </div>
        </div>
    }
}

/// The plan's fields: what, when (stepped), how long, how often, the agenda
/// and a note.
fn plan_fields(l: &ChatsLive) -> View {
    let c = &l.conversation;
    let shift = |m: i64| {
        l.on(move |w, _| {
            if let Some((_, MeetingForm::Plan { start_ms, .. })) = w.view.meeting_form.as_mut() {
                chats::shift_start(start_ms, m, unix_ms());
            }
        })
    };
    let when = {
        let form = c.form.clone();
        move || form.get().when
    };
    let durations = {
        let l = l.clone();
        move || -> View {
            let now = l.conversation.form.get().minutes;
            chats::DURATIONS
                .iter()
                .map(|&m| {
                    let tap = l.on(move |w, _| {
                        if let Some((_, MeetingForm::Plan { minutes, .. })) =
                            w.view.meeting_form.as_mut()
                        {
                            *minutes = m;
                        }
                    });
                    let variant = if now == m {
                        Variant::Primary
                    } else {
                        Variant::Secondary
                    };
                    view! { <Button variant={variant} size=Size::Sm onclick={tap}>{chats::duration_label(m)}</Button> }
                })
                .collect::<Vec<View>>()
                .into_view()
        }
    };
    let repeat = {
        let l = l.clone();
        move || -> View { repeat_view(&l, &l.conversation.form.get()) }
    };
    view! {
        <FieldText model={c.title.model.clone()} placeholder="What: Q4 planning" autocomplete="off" id="chats-plan-title" />
        <div style="display: flex; align-items: center; flex-wrap: wrap; gap: 8px">
            <Button size=Size::Sm onclick={shift(-1440)}>"−1 day"</Button>
            <Button size=Size::Sm onclick={shift(-15)}>"−15 min"</Button>
            <span style="flex: 1; min-width: 120px; text-align: center; color: var(--m-text-strong); font-weight: 600">{when}</span>
            <Button size=Size::Sm onclick={shift(15)}>"+15 min"</Button>
            <Button size=Size::Sm onclick={shift(1440)}>"+1 day"</Button>
        </div>
        <div style="display: flex; flex-wrap: wrap; gap: 8px">{durations}</div>
        {repeat}
        <FieldTextarea model={c.agenda.model.clone()} placeholder="Agenda, one item a line; a number at the end is its minutes\nBudget 10\nStage and sound 15" rows={3u32} id="chats-plan-agenda" />
        <FieldText model={c.text.model.clone()} placeholder="Anything else (optional)" autocomplete="off" id="chats-plan-text" />
    }
}

/// Repeats, and Until stepped a meeting at a time (`chats::repeat_fields`).
fn repeat_view(l: &ChatsLive, f: &FormV) -> View {
    let chips: Vec<View> = chats::REPEATS
        .iter()
        .map(|&(label, rule)| {
            let tap = l.on(move |w, _| {
                if let Some((
                    _,
                    MeetingForm::Plan {
                        repeat, until_ms, ..
                    },
                )) = w.view.meeting_form.as_mut()
                {
                    chats::pick_repeat(repeat, until_ms, rule);
                }
            });
            let variant = if f.repeat == rule {
                Variant::Primary
            } else {
                Variant::Secondary
            };
            view! { <Button variant={variant} size=Size::Sm onclick={tap}>{label}</Button> }
        })
        .collect();
    let until: View = if f.repeat.is_some() {
        let step = |s: UntilStep| {
            l.on(move |w, _| {
                if let Some((
                    _,
                    MeetingForm::Plan {
                        start_ms,
                        repeat,
                        until_ms,
                        ..
                    },
                )) = w.view.meeting_form.as_mut()
                {
                    chats::step_until(*start_ms, *repeat, until_ms, s);
                }
            })
        };
        let no_end = if f.until.is_none() {
            Variant::Primary
        } else {
            Variant::Secondary
        };
        let moves: View = match &f.until {
            None => {
                view! { <Button size=Size::Sm onclick={step(UntilStep::SetEnd)}>"Set an end"</Button> }
            }
            Some(label) => {
                let label = label.clone();
                view! {
                    <Button size=Size::Sm onclick={step(UntilStep::Earlier)}>"−1"</Button>
                    <span style="color: var(--m-text); font-size: var(--m-font-size-sm)">{label}</span>
                    <Button size=Size::Sm onclick={step(UntilStep::Later)}>"+1"</Button>
                }
            }
        };
        view! {
            <div style="display: flex; align-items: center; flex-wrap: wrap; gap: 8px">
                <span style={LABEL}>"Until"</span>
                <Button variant={no_end} size=Size::Sm onclick={step(UntilStep::NoEnd)}>"No end"</Button>
                {moves}
            </div>
        }
    } else {
        View::Empty
    };
    view! {
        <span style={LABEL}>"Repeats"</span>
        <div style="display: flex; flex-wrap: wrap; gap: 8px">{chips}</div>
        {until}
    }
}

/// The poll's fields: the question, the options, and whether more than one
/// may be chosen.
fn poll_fields(l: &ChatsLive) -> View {
    let c = &l.conversation;
    let set_multiple = {
        let m = c.multiple.clone();
        l.later(move |w, _| {
            let v = m.get_untracked();
            if let Some((_, MeetingForm::Poll { multiple, .. })) = w.view.meeting_form.as_mut() {
                *multiple = v;
            }
        })
    };
    let onchange: Rc<dyn Fn(&bool)> = Rc::new(move |_: &bool| set_multiple());
    view! {
        <FieldText model={c.question.model.clone()} placeholder="Question: which day suits?" autocomplete="off" id="chats-poll-question" />
        <FieldTextarea model={c.options.model.clone()} placeholder="Options, one a line\nFriday\nSaturday" rows={3u32} id="chats-poll-options" />
        <FieldCheckbox model={c.multiple.clone()} label="People may choose more than one" onchange={Some(onchange)} id="chats-poll-multiple" />
    }
}
