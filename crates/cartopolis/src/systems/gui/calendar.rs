//! The calendar: every meeting and meet-up the person is in, from all their
//! private chats, groups and community topics, in one place
//! (`docs/mockups/calendar`, v1 and v2).
//!
//! # Built from the chats, kept on this device
//!
//! Nothing new is sent to make a calendar: the plans are the sealed chat
//! messages already there — a meeting is a `cartopolis_core::meeting::Plan`,
//! a meet-up a `Point` with a time — and the answers to them. Answering or
//! skipping from here sends the same message the chat's card sends.
//!
//! **The app keeps no chat history across a restart** ([`ChatState`]'s lines
//! live in memory), so a plan seen last week would be gone. [`Book`] is the
//! calendar's own copy of the lines it needs: every plan, update, answer and
//! meet-up, in `platform::storage` beside the chat keys and as private as they
//! are. Lines are added as they arrive ([`collect_calendar`]); a plan whose
//! last date ended more than [`KEEP_PAST_MS`] ago is dropped with its answers.
//! The web page builds its calendar from the chat history it already keeps.
//!
//! # Reminders
//!
//! While the app runs ([`drive_reminder`]): ten minutes before a meeting the
//! person said Going or Maybe to, and for a meet-up they are going to, when it
//! is time to leave — the start, less the walk from where the map is, less
//! five minutes. The walk is guessed from the straight line, not routed:
//! close enough to say "leave now", and free. Nothing reminds while the app
//! is closed (decided 2026-10-19).

use std::collections::HashSet;

use crate::utils::clock::unix_ms;
use bevy::prelude::*;
use cartopolis_core::meeting::{
    current_plan, tally_rsvps, AnswerFor, Every, MeetingObject, Plan, Repeat, Rsvp,
};
use cartopolis_core::point::Point;
use serde::{Deserialize, Serialize};

use crate::systems::chat::ChatState;
use crate::systems::icons;

/// Where the book is kept, per server and account like the chat keys. On the
/// web this prefix is preloaded before the app starts (`crate::run`).
pub const STORE_PREFIX: &str = "calendar/";

/// How long a finished plan stays in the book: last month's meetings are still
/// easy to find again, and their notes are in their chats.
pub const KEEP_PAST_MS: u64 = 30 * DAY_MS;

/// Most lines the book keeps. Answers are most of them; a few busy
/// communities fit many times over.
const MAX_ENTRIES: usize = 5000;

const MIN_MS: u64 = 60_000;
const DAY_MS: u64 = 24 * 60 * MIN_MS;

/// How long before a meeting the reminder comes, and Join opens.
pub const MEETING_LEAD_MS: u64 = 10 * MIN_MS;

/// A meet-up has no end; it is drawn and counted as an hour.
const MEETUP_LENGTH_MS: u64 = 60 * MIN_MS;

/// Walking pace for "time to leave", with the straight line stretched by a
/// third for the streets in between.
const WALK_M_PER_S: f64 = 1.3;
const DETOUR: f64 = 1.33;

/// One line a calendar needs, as it arrived in its chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The chat's channel key.
    pub channel: String,
    /// Who said it, as the chat shows them.
    pub sender: String,
    /// When it arrived (Unix ms): the order answers are counted in.
    pub at_ms: u64,
    /// The plan, answer, update or meet-up, as the ActivityStreams JSON it
    /// travelled as.
    pub json: String,
}

/// What the person chose on the calendar's settings page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Plans answered Can't come stay on the list, faded.
    pub show_declined: bool,
    /// The "Coming up" card on the Community list.
    pub coming_up: bool,
    /// Remind when it is time to leave for a meet-up.
    pub leave_reminder: bool,
    /// Remind ten minutes before a meeting.
    pub meeting_reminder: bool,
    /// Communities (by id) whose plans show only once answered: a big one
    /// would otherwise fill the calendar.
    pub answered_only: Vec<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            show_declined: true,
            coming_up: true,
            leave_reminder: true,
            meeting_reminder: true,
            answered_only: Vec::new(),
        }
    }
}

/// The calendar's own copy of the lines it is built from, and the settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Book {
    pub entries: Vec<Entry>,
    pub prefs: Prefs,
    /// A hash of every entry, so a line the chats hold is found in the book
    /// without comparing it with each entry. Rebuilt after loading.
    #[serde(skip)]
    seen: HashSet<u64>,
    /// Moves on every change to the entries: what [`Calendar`]'s rows are
    /// cached against.
    #[serde(skip)]
    version: u64,
}

impl PartialEq for Book {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries && self.prefs == other.prefs
    }
}

fn entry_hash(e: &Entry) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (&e.channel, &e.sender, e.at_ms, &e.json).hash(&mut h);
    h.finish()
}

/// A line read back.
enum Said {
    Meeting(MeetingObject),
    Meetup(Point),
}

fn read(json: &str) -> Option<Said> {
    if let Some(o) = MeetingObject::from_activity_json(json) {
        return Some(Said::Meeting(o));
    }
    Point::from_activity_json(json)
        .ok()
        .filter(|p| p.meetup.is_some())
        .map(Said::Meetup)
}

/// What a calendar row is.
#[derive(Debug, Clone, PartialEq)]
pub enum What {
    /// One date of a meeting, `plan` as its author last left it.
    Meeting { plan: Plan },
    /// A meet-up: a place and a time.
    Meetup { point: Point },
}

/// One row of the calendar: a meeting's date or a meet-up.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub channel: String,
    pub what: What,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Who planned or proposed it.
    pub author: String,
    /// This person's answer for this date. For a meet-up, Going or nothing.
    pub mine: Option<Rsvp>,
    pub going: Vec<String>,
    pub maybe: Vec<String>,
    pub no: Vec<String>,
    /// A date its planner took out of the series.
    pub skipped: bool,
}

impl Item {
    /// What names it: the meeting's title, the meet-up's place.
    #[must_use]
    pub fn title(&self) -> String {
        match &self.what {
            What::Meeting { plan } => plan.title.clone(),
            What::Meetup { point } => point
                .name
                .clone()
                .or_else(|| point.text.clone())
                .unwrap_or_else(|| "Meet-up".into()),
        }
    }

    /// Which thing it is, to pick it again: the plan's id or the meet-up's
    /// place and time, and the date.
    #[must_use]
    pub fn pick(&self) -> Pick {
        let id = match &self.what {
            What::Meeting { plan } => plan.id.clone(),
            What::Meetup { point } => {
                format!("meetup:{:.5},{:.5}", point.latitude, point.longitude)
            }
        };
        Pick {
            channel: self.channel.clone(),
            id,
            date_ms: self.start_ms,
        }
    }

    #[must_use]
    pub fn is_meetup(&self) -> bool {
        matches!(self.what, What::Meetup { .. })
    }
}

/// A calendar row, picked: which chat, which plan, which date.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pick {
    pub channel: String,
    pub id: String,
    pub date_ms: u64,
}

impl Book {
    /// Mark the entries changed. Numbered across every book, so a book loaded
    /// or swapped in never matches rows cached for the one before it.
    pub fn touch(&mut self) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.version = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Add a line unless it is already here. `true` when it was new.
    pub fn add(&mut self, entry: Entry) -> bool {
        if self.seen.len() != self.entries.len() {
            self.seen = self.entries.iter().map(entry_hash).collect();
        }
        if !self.seen.insert(entry_hash(&entry)) {
            return false;
        }
        self.entries.push(entry);
        self.touch();
        true
    }

    /// Drop what has been over for longer than [`KEEP_PAST_MS`]: plans whose
    /// last date ended then, the answers and updates that name them, and old
    /// meet-ups. `offset_s` as for [`Plan::dates`].
    pub fn prune(&mut self, now_ms: u64, offset_s: &dyn Fn(u64) -> i64) {
        let cutoff = now_ms.saturating_sub(KEEP_PAST_MS);
        let mut live: HashSet<String> = HashSet::new();
        let mut dead: HashSet<String> = HashSet::new();
        for e in &self.entries {
            if let Some(Said::Meeting(MeetingObject::Plan(p))) = read(&e.json) {
                let p = self.current(&e.channel, &p, &e.sender);
                let last = match p.repeat {
                    None => Some(p.start_ms),
                    Some(Repeat { until_ms, .. }) => until_ms.map(|u| {
                        p.dates(p.start_ms, u + 1, offset_s)
                            .last()
                            .copied()
                            .unwrap_or(p.start_ms)
                    }),
                };
                match last {
                    Some(l) if l + p.duration_ms() < cutoff => dead.insert(p.id.clone()),
                    _ => live.insert(p.id.clone()),
                };
            }
        }
        self.entries.retain(|e| match read(&e.json) {
            Some(Said::Meeting(o)) => match &o {
                MeetingObject::Plan(p) | MeetingObject::Update(p) => {
                    !dead.contains(&p.id) || live.contains(&p.id)
                }
                MeetingObject::Answer { plan, .. } => !dead.contains(plan) || live.contains(plan),
                _ => false,
            },
            Some(Said::Meetup(p)) => p
                .meetup
                .is_some_and(|m| m.start_ms + MEETUP_LENGTH_MS >= cutoff),
            None => false,
        });
        let excess = self.entries.len().saturating_sub(MAX_ENTRIES);
        self.entries.drain(..excess);
        self.seen.clear();
        self.touch();
    }

    /// `plan` as `author` last left it, from the updates in its chat.
    fn current(&self, channel: &str, plan: &Plan, author: &str) -> Plan {
        let said: Vec<(String, MeetingObject)> = self
            .entries
            .iter()
            .filter(|e| e.channel == channel)
            .filter_map(|e| match read(&e.json) {
                Some(Said::Meeting(o @ MeetingObject::Update(_))) => Some((e.sender.clone(), o)),
                _ => None,
            })
            .collect();
        current_plan(plan, author, said.iter().map(|(w, o)| (w.as_str(), o)))
    }

    /// The rows starting in `from_ms..to_ms`, earliest first, for the person
    /// called `me` in their chats.
    #[must_use]
    pub fn items(
        &self,
        from_ms: u64,
        to_ms: u64,
        me: &str,
        offset_s: &dyn Fn(u64) -> i64,
    ) -> Vec<Item> {
        let mut said: Vec<(&Entry, Said)> = self
            .entries
            .iter()
            .filter_map(|e| read(&e.json).map(|s| (e, s)))
            .collect();
        said.sort_by_key(|(e, _)| e.at_ms);
        let mut out = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for (e, s) in &said {
            let in_chat = || {
                said.iter()
                    .filter(|(o, _)| o.channel == e.channel)
                    .filter_map(|(o, s)| match s {
                        Said::Meeting(m) => Some((o.sender.as_str(), m)),
                        Said::Meetup(_) => None,
                    })
            };
            match s {
                Said::Meeting(MeetingObject::Plan(p)) => {
                    if !seen.insert((e.channel.clone(), p.id.clone())) {
                        continue;
                    }
                    let plan = current_plan(p, &e.sender, in_chat());
                    let length = plan.duration_ms();
                    for date in plan.dates(from_ms.saturating_sub(length), to_ms, offset_s) {
                        if date < from_ms && date + length <= from_ms {
                            continue;
                        }
                        let answers = tally_rsvps(&plan, date, in_chat());
                        let who = |r: Rsvp| -> Vec<String> {
                            answers
                                .iter()
                                .filter(|(_, a)| *a == r)
                                .map(|(w, _)| w.clone())
                                .collect()
                        };
                        out.push(Item {
                            channel: e.channel.clone(),
                            start_ms: date,
                            end_ms: date + length,
                            author: e.sender.clone(),
                            mine: answers.iter().find(|(w, _)| w == me).map(|(_, r)| *r),
                            going: who(Rsvp::Going),
                            maybe: who(Rsvp::Maybe),
                            no: who(Rsvp::No),
                            skipped: plan.is_skipped(date),
                            what: What::Meeting { plan: plan.clone() },
                        });
                    }
                }
                Said::Meetup(point) => {
                    let Some(m) = point.meetup.filter(|m| !m.going) else {
                        continue;
                    };
                    let key = format!(
                        "{}@{:.5},{:.5}",
                        m.start_ms, point.latitude, point.longitude
                    );
                    if !(from_ms.saturating_sub(MEETUP_LENGTH_MS)..to_ms).contains(&m.start_ms)
                        || !seen.insert((e.channel.clone(), key))
                    {
                        continue;
                    }
                    let mut going = vec![e.sender.clone()];
                    for (o, s) in &said {
                        if let Said::Meetup(p) = s {
                            if o.channel == e.channel
                                && p.meetup.is_some_and(|m| m.going)
                                && p.same_meetup(point)
                                && !going.contains(&o.sender)
                            {
                                going.push(o.sender.clone());
                            }
                        }
                    }
                    out.push(Item {
                        channel: e.channel.clone(),
                        start_ms: m.start_ms,
                        end_ms: m.start_ms + MEETUP_LENGTH_MS,
                        author: e.sender.clone(),
                        mine: going.iter().any(|w| w == me).then_some(Rsvp::Going),
                        going,
                        maybe: Vec::new(),
                        no: Vec::new(),
                        skipped: false,
                        what: What::Meetup {
                            point: point.clone(),
                        },
                    });
                }
                Said::Meeting(_) => {}
            }
        }
        out.sort_by(|a, b| {
            a.start_ms
                .cmp(&b.start_ms)
                .then_with(|| a.title().cmp(&b.title()))
        });
        out
    }
}

/// This device's UTC offset at an instant, in seconds: what [`Plan::dates`]
/// keeps a series' clock time in.
#[must_use]
pub fn offset_s(at_ms: u64) -> i64 {
    jiff::Timestamp::from_millisecond(at_ms as i64)
        .map(|t| i64::from(jiff::tz::TimeZone::system().to_offset(t).seconds()))
        .unwrap_or(0)
}

/// Days since 1970-01-01 on this device's calendar.
#[must_use]
pub fn local_day(at_ms: u64) -> i64 {
    (at_ms as i64 / 1000 + offset_s(at_ms)).div_euclid(86_400)
}

/// The instant a local day starts.
#[must_use]
pub fn day_start_ms(day: i64) -> u64 {
    let guess = day * 86_400;
    let at = guess - offset_s(u64::try_from(guess.max(0)).unwrap_or(0) * 1000);
    u64::try_from(at.max(0)).unwrap_or(0) * 1000
}

fn civil(day: i64) -> jiff::civil::Date {
    let base = jiff::civil::date(1970, 1, 1);
    base.checked_add(jiff::Span::new().days(day))
        .unwrap_or(base)
}

/// "Thursday 22 October".
#[must_use]
pub fn long_day(at_ms: u64) -> String {
    civil(local_day(at_ms)).strftime("%A %-d %B").to_string()
}

/// "Thu 22 Oct".
#[must_use]
pub fn short_day(at_ms: u64) -> String {
    civil(local_day(at_ms)).strftime("%a %-d %b").to_string()
}

/// "Today", "Tomorrow" or the long day.
#[must_use]
pub fn day_heading(at_ms: u64, now_ms: u64) -> String {
    match local_day(at_ms) - local_day(now_ms) {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        _ => long_day(at_ms),
    }
}

/// How a series comes back, in words: "Every Thursday", "Every 2 weeks on
/// Thursday", "Every month on the 4th Thursday", and its end if it has one.
#[must_use]
pub fn rule_words(plan: &Plan) -> Option<String> {
    let r = plan.repeat?;
    let date = civil(local_day(plan.start_ms));
    let weekday = date.strftime("%A").to_string();
    let mut s = match (r.every, r.interval) {
        (Every::Week, 1) => format!("Every {weekday}"),
        (Every::Week, n) => format!("Every {n} weeks on {weekday}"),
        (Every::Month, n) => {
            let nth = ["1st", "2nd", "3rd", "4th", "5th"][((date.day() - 1) / 7) as usize];
            if n == 1 {
                format!("Every month on the {nth} {weekday}")
            } else {
                format!("Every {n} months on the {nth} {weekday}")
            }
        }
    };
    if let Some(u) = r.until_ms {
        s.push_str(&format!(
            " until {}",
            long_day(u)
                .split_once(' ')
                .map_or(String::new(), |(_, d)| d.to_string())
        ));
    }
    Some(s)
}

/// "3rd of 9", or "3rd" for a series with no end.
#[must_use]
pub fn ordinal_in_series(plan: &Plan, date_ms: u64) -> Option<String> {
    plan.repeat?;
    let n = plan.dates(plan.start_ms, date_ms + 1, &offset_s).len();
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    Some(match plan.date_count(&offset_s) {
        Some(total) => format!("{n}{suffix} of {total}"),
        None => format!("{n}{suffix}"),
    })
}

/// Straight-line metres between two (lat, lon).
fn metres(a: (f64, f64), b: (f64, f64)) -> f64 {
    cartopolis_core::geo::plane_distance_m([a.0, a.1], [b.0, b.1])
}

/// Minutes to walk from `here` to the meet-up, guessed from the straight line.
#[must_use]
pub fn walk_minutes(here: (f64, f64), point: &Point) -> u64 {
    let m = metres(here, (point.latitude, point.longitude)) * DETOUR;
    (m / WALK_M_PER_S / 60.0).ceil().max(1.0) as u64
}

// ── Bevy ────────────────────────────────────────────────────────────────────

/// Which page of the calendar is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    List,
    Month,
    /// One row, opened.
    Detail,
    /// Plan something: what, and in which chat.
    New,
    Settings,
}

/// What the "+" page is planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewKind {
    #[default]
    Meeting,
    Meetup,
}

/// A reminder on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Reminder {
    pub item: Item,
    pub title: String,
    pub line: String,
}

impl Reminder {
    /// The icon beside it: walking to a meet-up, or a call.
    #[must_use]
    pub fn icon(&self) -> &'static str {
        if self.item.is_meetup() {
            icons::WALK
        } else {
            icons::VIDEOCAM
        }
    }

    /// Its main verb: Walk there to a meet-up, Join a meeting.
    #[must_use]
    pub fn go_label(&self) -> &'static str {
        if self.item.is_meetup() {
            "Walk there"
        } else {
            "Join"
        }
    }
}

/// A tap on the reminder: one set, for both editions, done by
/// [`reminder_act`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderAction {
    /// Its chat, in the Chats panel.
    OpenChat,
    /// Walk there (a meet-up) or Join (a meeting).
    Go,
    Close,
}

/// Do a tap on the library's reminder (`lib_reminder`). Every tap puts the
/// reminder away.
pub fn reminder_act(
    cal: &mut Calendar,
    action: ReminderAction,
    view: &mut crate::systems::chats::ChatsView,
    right_panel: &mut crate::resources::RightPanel,
    nav: &mut crate::systems::geo_nav::GeoNavState,
    calls: &mut crate::systems::calls::Calls,
) {
    let Some(r) = cal.reminder.take() else {
        return;
    };
    match action {
        ReminderAction::OpenChat => {
            view.open_chat(&r.item.channel);
            *right_panel = crate::resources::RightPanel::Chat;
        }
        ReminderAction::Go => match &r.item.what {
            What::Meetup { point } => {
                nav.route_to(
                    point.name.clone().unwrap_or_else(|| "Meet-up".into()),
                    point.latitude,
                    point.longitude,
                );
            }
            What::Meeting { .. } => {
                if let Some(group) = crate::systems::private_chat::group_of(&r.item.channel) {
                    calls.request = Some(crate::systems::calls::CallRequest::Join(group));
                }
            }
        },
        ReminderAction::Close => {}
    }
}

/// The calendar's state: the book, which account it belongs to, and what is
/// on screen.
#[derive(Resource, Default)]
pub struct Calendar {
    pub book: Book,
    /// The storage key the book was loaded from; `None` until loaded.
    key: Option<String>,
    /// The chats' shape when last read, to read them again only on a change.
    scanned: Vec<(String, usize)>,
    dirty: bool,
    /// Who this device's person is in the chats.
    pub me: String,
    /// The calendar is open in the Community panel.
    pub open: bool,
    pub page: Page,
    /// The day the list or month is on, as [`local_day`].
    pub day: Option<i64>,
    /// The month the grid shows: (year, month).
    pub month: Option<(i16, i8)>,
    /// Scroll the list to [`Self::day`] on the next draw.
    pub scroll_to_day: bool,
    /// The row opened on [`Page::Detail`].
    pub picked: Option<Pick>,
    /// The opened meeting's answer goes to every date of its series.
    pub every: bool,
    pub new_kind: NewKind,
    pub new_chat: Option<String>,
    /// Where the map is, for "time to leave".
    pub here: Option<(f64, f64)>,
    /// Rows already reminded about.
    reminded: HashSet<Pick>,
    pub reminder: Option<Reminder>,
    /// A tap on the library's reminder (`lib_reminder`), done by
    /// [`drive_reminder`].
    pub lib_action: Option<ReminderAction>,
    /// When the reminders were last worked out (ms).
    checked_ms: u64,
    /// Every row from a month back to a year ahead, worked out once per change
    /// to the book: reading the book parses every line it holds, which is far
    /// too much to do on every frame of a phone.
    rows: std::sync::Mutex<Option<Rows>>,
}

struct Rows {
    version: u64,
    me: String,
    from_ms: u64,
    to_ms: u64,
    items: std::sync::Arc<Vec<Item>>,
}

/// How far ahead [`Calendar`]'s cached rows reach.
const ROWS_AHEAD_MS: u64 = 400 * DAY_MS;

impl Calendar {
    /// Rows from `from_ms` to `to_ms`, worked out from the book, from the
    /// cache when it covers them.
    fn rows(&self, from_ms: u64, to_ms: u64) -> Vec<Item> {
        let now = unix_ms();
        let mut rows = self
            .rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fresh = rows.as_ref().is_some_and(|r| {
            r.version == self.book.version
                && r.me == self.me
                && now < r.from_ms + KEEP_PAST_MS + DAY_MS
        });
        if !fresh {
            let from = now.saturating_sub(KEEP_PAST_MS + DAY_MS);
            *rows = Some(Rows {
                version: self.book.version,
                me: self.me.clone(),
                from_ms: from,
                to_ms: now + ROWS_AHEAD_MS,
                items: std::sync::Arc::new(self.book.items(
                    from,
                    now + ROWS_AHEAD_MS,
                    &self.me,
                    &offset_s,
                )),
            });
        }
        let r = rows.as_ref().expect("filled above");
        if from_ms < r.from_ms || to_ms > r.to_ms {
            // A month browsed far back or ahead: worked out for the asking.
            return self.book.items(from_ms, to_ms, &self.me, &offset_s);
        }
        r.items
            .iter()
            .filter(|i| i.start_ms < to_ms && i.end_ms > from_ms)
            .cloned()
            .collect()
    }

    /// Rows from `from_ms` to `to_ms`, as the settings allow: Can't come
    /// hidden if asked, and a community's unanswered plans if asked.
    #[must_use]
    pub fn visible(
        &self,
        from_ms: u64,
        to_ms: u64,
        communities: &crate::systems::communities::Communities,
    ) -> Vec<Item> {
        let prefs = &self.book.prefs;
        self.rows(from_ms, to_ms)
            .into_iter()
            .filter(|i| prefs.show_declined || i.mine != Some(Rsvp::No))
            .filter(|i| {
                i.mine.is_some()
                    || communities
                        .topic(&i.channel)
                        .is_none_or(|(c, _)| !prefs.answered_only.contains(&c.id.to_string()))
            })
            .collect()
    }

    /// The row a [`Pick`] names, worked out again from the book.
    #[must_use]
    pub fn find(&self, pick: &Pick) -> Option<Item> {
        self.rows(pick.date_ms, pick.date_ms + 1)
            .into_iter()
            .find(|i| i.pick() == *pick)
    }

    /// A picked row's title, for the ‹ Back chip of its Show in world
    /// (`bridges`).
    #[must_use]
    pub fn title_of(&self, pick: &Pick) -> Option<String> {
        self.find(pick).map(|i| i.title().to_string())
    }

    /// Open the calendar on today's list.
    pub fn show(&mut self, now_ms: u64) {
        self.open = true;
        self.page = Page::List;
        self.day = Some(local_day(now_ms));
        self.scroll_to_day = true;
    }

    // What a tap on the calendar's pages changes, for egui's pages and the
    // library's (`lib_calendar`) alike.

    /// Open a row on its own page.
    pub fn open_item(&mut self, pick: Pick) {
        self.picked = Some(pick);
        self.every = false;
        self.page = Page::Detail;
    }

    /// List or Month, the switch over the overview.
    pub fn switch_view(&mut self, page: Page) {
        if page != self.page {
            self.page = page;
            self.scroll_to_day = true;
        }
    }

    /// A day tapped in the week strip or the month.
    pub fn pick_day(&mut self, day: i64) {
        self.day = Some(day);
        self.scroll_to_day = true;
    }

    /// The month the grid shows: the one browsed to, else the chosen day's.
    #[must_use]
    pub fn shown_month(&self, now_ms: u64) -> (i16, i8) {
        self.month.unwrap_or_else(|| {
            let d = civil(self.day.unwrap_or_else(|| local_day(now_ms)));
            (d.year(), d.month())
        })
    }

    /// The previous (`-1`) or next (`1`) month.
    pub fn step_month(&mut self, dir: i32, now_ms: u64) {
        let (y, m) = self.shown_month(now_ms);
        let n = i32::from(y) * 12 + i32::from(m) - 1 + dir;
        self.month = Some(((n.div_euclid(12)) as i16, (n.rem_euclid(12) + 1) as i8));
    }

    /// "+": plan something.
    pub fn plan_new(&mut self) {
        self.page = Page::New;
        self.new_chat = None;
    }

    /// The day the "+" page plans on: the chosen day, never before today.
    #[must_use]
    pub fn new_day(&self, now_ms: u64) -> i64 {
        self.day
            .unwrap_or_else(|| local_day(now_ms))
            .max(local_day(now_ms))
    }

    /// The "+" page's Next: the chat's own form for a meeting, or its
    /// meet-up, on the day chosen.
    pub fn new_next(&mut self, now_ms: u64, out: &mut Out) {
        let day = self.new_day(now_ms);
        if let Some(key) = self.new_chat.take() {
            match self.new_kind {
                NewKind::Meeting => out.plan_in = Some((key, day_start_ms(day))),
                NewKind::Meetup => out.meetup_in = Some(key),
            }
            self.page = Page::List;
        }
    }

    /// Limit a community to the plans answered, or stop limiting it.
    pub fn set_answered_only(&mut self, community: &str, only: bool) {
        let list = &mut self.book.prefs.answered_only;
        list.retain(|x| x != community);
        if only {
            list.push(community.to_string());
        }
    }

    /// How many plans and meet-ups the book keeps.
    #[must_use]
    pub fn plans_kept(&self) -> usize {
        self.book
            .entries
            .iter()
            .filter(|e| {
                matches!(
                    read(&e.json),
                    Some(Said::Meeting(MeetingObject::Plan(_)) | Said::Meetup(_))
                )
            })
            .count()
    }
}

/// What the settings say about the book.
#[must_use]
pub fn plans_note(plans: usize) -> String {
    format!(
        "{plans} plans are kept here only. Your other devices build their own calendar from their chats."
    )
}

/// An answer to a meeting's date, from its page: this date, every date, or
/// the only one.
#[must_use]
pub fn answer(item: &Item, plan: &Plan, rsvp: Rsvp, every: bool) -> (String, MeetingObject) {
    let date = if every {
        AnswerFor::Every
    } else if plan.repeat.is_none() {
        AnswerFor::First
    } else {
        AnswerFor::Date(item.start_ms)
    };
    (
        item.channel.clone(),
        MeetingObject::Answer {
            plan: plan.id.clone(),
            rsvp,
            date,
        },
    )
}

/// The planner takes this date out of the series, or puts it back.
#[must_use]
pub fn toggle_skip(item: &Item, plan: &Plan) -> (String, MeetingObject) {
    let mut p = plan.clone();
    if item.skipped {
        p.skip.retain(|d| *d != item.start_ms);
    } else {
        p.skip.push(item.start_ms);
    }
    (item.channel.clone(), MeetingObject::Update(p))
}

/// The planner ends the series with this date.
#[must_use]
pub fn stop_after(item: &Item, plan: &Plan) -> (String, MeetingObject) {
    let mut p = plan.clone();
    if let Some(r) = p.repeat.as_mut() {
        r.until_ms = Some(item.start_ms);
    }
    p.skip.retain(|d| *d <= item.start_ms);
    (item.channel.clone(), MeetingObject::Update(p))
}

/// The meeting is about to start or going on: Join works.
#[must_use]
pub fn is_live(item: &Item, now_ms: u64) -> bool {
    now_ms + MEETING_LEAD_MS >= item.start_ms && now_ms <= item.end_ms && !item.skipped
}

/// Join's words: "Join", or from when it can be pressed.
#[must_use]
pub fn join_label(item: &Item, now_ms: u64) -> String {
    if is_live(item, now_ms) {
        "Join".to_string()
    } else {
        format!(
            "Join · from {}",
            crate::systems::chats::clock_time(item.start_ms.saturating_sub(MEETING_LEAD_MS))
        )
    }
}

/// When it is, on its page: "Today, 18:00", a meeting with its end.
#[must_use]
pub fn when_line(item: &Item, now_ms: u64) -> String {
    let clock = crate::systems::chats::clock_time;
    if item.is_meetup() {
        format!(
            "{}, {}",
            day_heading(item.start_ms, now_ms),
            clock(item.start_ms)
        )
    } else {
        format!(
            "{}, {} – {}",
            day_heading(item.start_ms, now_ms),
            clock(item.start_ms),
            clock(item.end_ms)
        )
    }
}

/// How a series comes back and which date this is: "Every Thursday · 3rd of 9".
#[must_use]
pub fn series_line(item: &Item) -> Option<String> {
    let What::Meeting { plan } = &item.what else {
        return None;
    };
    let rule = rule_words(plan)?;
    let nth = ordinal_in_series(plan, item.start_ms)
        .map(|n| format!(" · {n}"))
        .unwrap_or_default();
    Some(format!("{rule}{nth}"))
}

/// Where it came from and who: "Fietsclub · planned by you".
#[must_use]
pub fn by_line(item: &Item, from: &str, me: &str) -> String {
    let by = if item.author == me {
        "you"
    } else {
        item.author.as_str()
    };
    let verb = if item.is_meetup() {
        "proposed"
    } else {
        "planned"
    };
    format!("{from} · {verb} by {by}")
}

/// A row is drawn faded: skipped, can't come, or over.
#[must_use]
pub fn row_faded(item: &Item, now_ms: u64) -> bool {
    item.skipped || item.mine == Some(Rsvp::No) || item.end_ms <= now_ms
}

/// The small line under a row's time: the day given, or a meeting's length.
#[must_use]
pub fn row_under(item: &Item, day: Option<&str>) -> Option<String> {
    day.map(str::to_string).or_else(|| {
        (!item.is_meetup()).then(|| {
            let mins = (item.end_ms - item.start_ms) / MIN_MS;
            if mins % 60 == 0 {
                format!("{} h", mins / 60)
            } else if mins > 60 {
                format!("{}h{:02}", mins / 60, mins % 60)
            } else {
                format!("{mins} min")
            }
        })
    })
}

/// A row's second line, after its icon: the chat, how many go, that it
/// repeats, or who skipped it.
#[must_use]
pub fn row_sub(item: &Item, chat_label: &str) -> String {
    if item.skipped {
        return format!("Skipped by {}", item.author);
    }
    match &item.what {
        What::Meetup { .. } if item.going.len() > 1 => {
            format!("{chat_label} · {} going", item.going.len())
        }
        What::Meeting { plan } if plan.repeat.is_some() => format!("repeats · {chat_label}"),
        _ => chat_label.to_string(),
    }
}

/// Which kinds a day holds: (a meeting, a meet-up).
#[must_use]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub fn day_kinds(items: &[Item], day: i64) -> (bool, bool) {
    let on = |meetup: bool| {
        items
            .iter()
            .any(|i| local_day(i.start_ms) == day && i.is_meetup() == meetup)
    };
    (on(false), on(true))
}

/// A month's grid, Monday first as Dutch calendars are: its first cell's
/// day, and how many weeks it spans.
#[must_use]
pub fn month_grid(year: i16, month: i8) -> (i64, i64) {
    let first = jiff::civil::date(year, month, 1);
    let first_day = first
        .since(jiff::civil::date(1970, 1, 1))
        .map_or(0, |s| i64::from(s.get_days()));
    let lead = i64::from(first.weekday().to_monday_zero_offset());
    let weeks = (lead + i64::from(first.days_in_month()) + 6) / 7;
    (first_day - lead, weeks)
}

/// "November 2026".
#[must_use]
pub fn month_title(year: i16, month: i8) -> String {
    jiff::civil::date(year, month, 1)
        .strftime("%B %Y")
        .to_string()
}

/// A day's weekday, short ("Thu"), its number in the month, and its month.
#[must_use]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub fn day_face(day: i64) -> (String, i8, i8) {
    let d = civil(day);
    (d.strftime("%a").to_string(), d.day(), d.month())
}

fn store_key(server: &str, username: &str) -> String {
    let clean = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!("{STORE_PREFIX}{}/{}.json", clean(server), clean(username))
}

/// Keep the book: load it for the account signed in, copy into it every plan,
/// answer, update and meet-up the chats hold, and save it when that changed.
pub fn collect_calendar(
    chat: Res<ChatState>,
    identity: Res<crate::resources::AuthIdentity>,
    mut cal: ResMut<Calendar>,
) {
    use crate::platform::storage::{self, Namespace};
    let (Some(user), Some(server)) = (
        identity.username.as_deref(),
        identity.server_name.as_deref(),
    ) else {
        return;
    };
    let key = store_key(server, user);
    if cal.key.as_deref() != Some(key.as_str()) {
        match storage::read_now(Namespace::Data, &key) {
            storage::Cached::Loading => return,
            storage::Cached::Ready(bytes) => {
                cal.book = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "calendar unreadable; starting an empty one");
                    Book::default()
                });
            }
            _ => cal.book = Book::default(),
        }
        cal.book.touch();
        cal.key = Some(key.clone());
        cal.me = user.to_string();
        cal.scanned.clear();
        cal.reminded.clear();
    }
    // Read the chats again only when one of them grew or shrank.
    let shape: Vec<(String, usize)> = chat
        .channels
        .iter()
        .filter(|c| crate::systems::private_chat::group_of(&c.key).is_some())
        .map(|c| (c.key.clone(), c.lines.len()))
        .collect();
    if shape != cal.scanned {
        let mut added = false;
        for c in chat
            .channels
            .iter()
            .filter(|c| crate::systems::private_chat::group_of(&c.key).is_some())
        {
            for l in c.lines.iter().filter(|l| !l.removed) {
                let json = match (&l.meeting, &l.point) {
                    (Some(m), _) => Some(m.to_activity_json()),
                    (None, Some(p)) if p.meetup.is_some() => p.to_activity_json(None).ok(),
                    _ => None,
                };
                let Some(json) = json else { continue };
                added |= cal.book.add(Entry {
                    channel: c.key.clone(),
                    sender: l.sender.clone(),
                    at_ms: l.at_ms,
                    json,
                });
            }
        }
        cal.scanned = shape;
        if added {
            cal.book.prune(unix_ms(), &offset_s);
            cal.dirty = true;
        }
    }
    if std::mem::take(&mut cal.dirty) {
        match serde_json::to_vec(&cal.book) {
            Ok(bytes) => {
                if let Err(e) = storage::write_now(Namespace::Data, &key, &bytes) {
                    tracing::warn!(error = %e, "could not save the calendar");
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not write the calendar"),
        }
    }
}

/// Where the map is, for the walk to a meet-up.
pub fn track_here(
    mut cal: ResMut<Calendar>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: crate::grid::AnchoredCamera,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    let here = camera
        .get()
        .zip(map_anchor.geo)
        .map(|((cell, tf), anchor)| crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf));
    if cal.here != here {
        cal.here = here;
    }
}

/// What the calendar's pages asked for, done by the Community panel
/// (`chats::drive_chats`), which owns the chats and the map.
#[derive(Default)]
pub struct Out {
    /// Open this chat.
    pub open_chat: Option<String>,
    /// Seal and send into a chat.
    pub meeting: Option<(String, MeetingObject)>,
    /// "I'm going" to a meet-up.
    pub meetup_going: Option<(String, Point)>,
    /// Walk to a place: (lat, lon, name).
    pub route_to: Option<(f64, f64, String)>,
    /// Look at a place.
    pub go_to: Option<Point>,
    /// Join a chat's call.
    pub join: Option<Vec<u8>>,
    /// Plan a meeting in this chat, on the day that starts then.
    pub plan_in: Option<(String, u64)>,
    /// Propose a meet-up in this chat.
    pub meetup_in: Option<String>,
    /// Leave the calendar for the Community list.
    pub back: bool,
    /// The settings changed: save the book.
    pub save: bool,
}

/// The chat a row came from, as the calendar names it: "Sanne", "Fietsclub",
/// "Gorecht buurt · # bankjes".
#[must_use]
pub fn chat_name(
    chat: &ChatState,
    communities: &crate::systems::communities::Communities,
    channel: &str,
) -> String {
    if let Some((c, t)) = communities.topic(channel) {
        return format!("{} · # {}", c.settings.name, t.name);
    }
    chat.channels.iter().find(|c| c.key == channel).map_or_else(
        || "a chat".to_string(),
        |c| crate::systems::chats::display_name(&c.key, &c.label),
    )
}

/// One row of the "Coming up" card: the row, the chat it came from, and
/// "today" or "tomorrow".
#[derive(Debug, Clone, PartialEq)]
pub struct Soon {
    pub item: Item,
    pub chat: String,
    pub day: &'static str,
}

/// What the "Coming up" card shows, for both editions (egui's
/// [`coming_up`], the library's `lib_chats`).
#[derive(Debug, Clone, PartialEq)]
pub struct ComingUp {
    /// "Coming up", "Nothing coming up", or "Plans" with the rows off.
    pub label: &'static str,
    pub soon: Vec<Soon>,
}

/// Up to two rows from the next day, unless the settings switched them off.
#[must_use]
pub fn coming_up_list(
    cal: &Calendar,
    chat: &ChatState,
    communities: &crate::systems::communities::Communities,
    now: u64,
) -> ComingUp {
    let rows = cal.book.prefs.coming_up;
    let soon: Vec<Soon> = if rows {
        cal.visible(now, now + DAY_MS, communities)
            .into_iter()
            .filter(|i| !i.skipped && i.mine != Some(Rsvp::No))
            .take(2)
            .map(|item| Soon {
                chat: chat_name(chat, communities, &item.channel),
                day: if local_day(item.start_ms) == local_day(now) {
                    "today"
                } else {
                    "tomorrow"
                },
                item,
            })
            .collect()
    } else {
        Vec::new()
    };
    let label = match (rows, soon.is_empty()) {
        (false, _) => "Plans",
        (true, true) => "Nothing coming up",
        (true, false) => "Coming up",
    };
    ComingUp { label, soon }
}

/// The word on a row's answer pill: Skipped, Going, Maybe, Can't come, or
/// Answer while this person has not.
#[must_use]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub fn answer_word(item: &Item) -> &'static str {
    if item.skipped {
        return "Skipped";
    }
    match item.mine {
        Some(Rsvp::Going) => "Going",
        Some(Rsvp::Maybe) => "Maybe",
        Some(Rsvp::No) => "Can't come",
        None => "Answer",
    }
}

/// Whether a row shows its answer pill: not for a past date nobody answered.
#[must_use]
pub fn shows_answer(item: &Item, now_ms: u64) -> bool {
    item.end_ms > now_ms || item.mine.is_some()
}

/// The reminder over whatever is open — time to leave for a meet-up, or a
/// meeting about to start: works out when one is due and when it goes, and
/// does the tap the library's reminder (`lib_reminder`) hands back.
#[allow(clippy::too_many_arguments)]
pub fn drive_reminder(
    mut cal: ResMut<Calendar>,
    mut view: ResMut<crate::systems::chats::ChatsView>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    mut calls: ResMut<crate::systems::calls::Calls>,
    chat: Res<ChatState>,
    communities: Res<crate::systems::communities::Communities>,
) {
    let now = unix_ms();
    // Worked out every few seconds: a reminder a few seconds late is on time.
    if now >= cal.checked_ms + 5_000 && cal.reminder.is_none() && cal.key.is_some() {
        cal.checked_ms = now;
        let prefs = cal.book.prefs.clone();
        let here = cal.here;
        let due = cal
            .visible(
                now.saturating_sub(MEETUP_LENGTH_MS),
                now + 3 * 3_600_000,
                &communities,
            )
            .into_iter()
            .filter(|i| !i.skipped && now < i.start_ms && !cal.reminded.contains(&i.pick()))
            .find_map(|i| match &i.what {
                What::Meeting { .. }
                    if prefs.meeting_reminder
                        && matches!(i.mine, Some(Rsvp::Going | Rsvp::Maybe))
                        && now + MEETING_LEAD_MS >= i.start_ms =>
                {
                    let mins = (i.start_ms - now).div_ceil(MIN_MS);
                    Some(Reminder {
                        title: format!("{} starts in {mins} min", i.title()),
                        line: chat_name(&chat, &communities, &i.channel),
                        item: i.clone(),
                    })
                }
                What::Meetup { point } if prefs.leave_reminder && i.mine.is_some() => {
                    let walk = here.map_or(25, |h| walk_minutes(h, point));
                    let leave = i.start_ms.saturating_sub((walk + 5) * MIN_MS);
                    (now >= leave).then(|| Reminder {
                        title: format!("Leave now for {}", i.title()),
                        line: format!(
                            "Meet-up at {} · about {walk} min walk",
                            crate::systems::chats::clock_time(i.start_ms)
                        ),
                        item: i.clone(),
                    })
                }
                _ => None,
            });
        if let Some(r) = due {
            cal.reminded.insert(r.item.pick());
            cal.reminder = Some(r);
        }
    }
    let Some(start_ms) = cal.reminder.as_ref().map(|r| r.item.start_ms) else {
        return;
    };
    // Gone once the meet-up or meeting has started.
    if now >= start_ms + 5 * MIN_MS {
        cal.reminder = None;
        return;
    }
    // Do the tap the library's reminder (`lib_reminder`) handed back.
    if let Some(a) = cal.lib_action {
        cal.lib_action = None;
        reminder_act(
            &mut cal,
            a,
            &mut view,
            &mut right_panel,
            &mut nav,
            &mut calls,
        );
    }
}

/// Save the book after the settings changed.
pub fn mark_dirty(cal: &mut Calendar) {
    cal.dirty = true;
}

/// The plans the scripted captures show (`--ui calendar…`), in the demo chats
/// `chats::seed_demo_chats` opens: a meet-up with Sanne this afternoon, a
/// weekly meeting tonight with one date skipped, and a few more this week.
fn demo_book(now: u64) -> Book {
    const H: u64 = 3_600_000;
    let base = now.div_ceil(15 * MIN_MS) * 15 * MIN_MS;
    let mut book = Book::default();
    let mut at = 0;
    let mut say = |book: &mut Book, channel: &str, sender: &str, json: String| {
        at += 1;
        book.add(Entry {
            channel: channel.into(),
            sender: sender.into(),
            at_ms: now - 3 * DAY_MS + at * MIN_MS,
            json,
        });
    };
    let meetup = |name: &str, lat: f64, lon: f64, start_ms: u64, going: bool| Point {
        latitude: lat,
        longitude: lon,
        name: Some(name.into()),
        meetup: Some(cartopolis_core::point::Meetup { start_ms, going }),
        ..Point::default()
    };
    let json = |p: &Point| p.to_activity_json(None).unwrap_or_default();
    let (sanne, joris, family, crew) = ("dm:5a4e", "dm:10a5", "dm:fa01", "dm:6d656574");
    let vismarkt = meetup("Vismarkt", 53.21648, 6.56651, base + 150 * MIN_MS, false);
    say(&mut book, sanne, "Sanne", json(&vismarkt));
    say(
        &mut book,
        sanne,
        "jeroen",
        json(&meetup(
            "Vismarkt",
            53.21648,
            6.56651,
            base + 150 * MIN_MS,
            true,
        )),
    );
    let weekly = Plan {
        id: "urn:uuid:demo-weekly".into(),
        title: "Buurtvergadering".into(),
        start_ms: base + 8 * H,
        end_ms: Some(base + 8 * H + 90 * MIN_MS),
        agenda: vec![
            cartopolis_core::meeting::AgendaItem {
                name: "Nieuwe bankjes".into(),
                minutes: 10,
            },
            cartopolis_core::meeting::AgendaItem {
                name: "Budget".into(),
                minutes: 15,
            },
        ],
        repeat: Some(Repeat {
            every: Every::Week,
            interval: 1,
            until_ms: None,
        }),
        ..Plan::default()
    };
    say(
        &mut book,
        crew,
        "Ann",
        MeetingObject::Plan(weekly.clone()).to_activity_json(),
    );
    let week = 7 * DAY_MS;
    let second = weekly.dates(
        weekly.start_ms + 1,
        weekly.start_ms + week + 2 * H,
        &offset_s,
    );
    if let Some(d) = second.first() {
        say(
            &mut book,
            crew,
            "Ann",
            MeetingObject::Update(Plan {
                skip: vec![*d],
                ..weekly.clone()
            })
            .to_activity_json(),
        );
    }
    for (who, rsvp, date) in [
        ("Bob", Rsvp::Going, AnswerFor::Every),
        ("Carol", Rsvp::Maybe, AnswerFor::First),
        ("Dave", Rsvp::No, AnswerFor::First),
    ] {
        say(
            &mut book,
            crew,
            who,
            MeetingObject::Answer {
                plan: weekly.id.clone(),
                rsvp,
                date,
            }
            .to_activity_json(),
        );
    }
    say(
        &mut book,
        joris,
        "Joris",
        json(&meetup(
            "Hoofdstation",
            53.21122,
            6.56446,
            base + DAY_MS + 4 * H,
            false,
        )),
    );
    let call = Plan {
        id: "urn:uuid:demo-family".into(),
        title: "Family call".into(),
        start_ms: base + 3 * DAY_MS + 2 * H,
        end_ms: Some(base + 3 * DAY_MS + 3 * H),
        ..Plan::default()
    };
    say(
        &mut book,
        family,
        "Mum",
        MeetingObject::Plan(call.clone()).to_activity_json(),
    );
    say(
        &mut book,
        family,
        "jeroen",
        MeetingObject::Answer {
            plan: call.id.clone(),
            rsvp: Rsvp::Going,
            date: AnswerFor::First,
        }
        .to_activity_json(),
    );
    let lunch = Plan {
        id: "urn:uuid:demo-lunch".into(),
        title: "Lunch planning".into(),
        start_ms: base + 5 * DAY_MS,
        end_ms: Some(base + 5 * DAY_MS + H),
        ..Plan::default()
    };
    say(
        &mut book,
        crew,
        "Ann",
        MeetingObject::Plan(lunch.clone()).to_activity_json(),
    );
    say(
        &mut book,
        crew,
        "jeroen",
        MeetingObject::Answer {
            plan: lunch.id.clone(),
            rsvp: Rsvp::No,
            date: AnswerFor::First,
        }
        .to_activity_json(),
    );
    say(
        &mut book,
        sanne,
        "Sanne",
        json(&meetup(
            "Noorderplantsoen",
            53.22455,
            6.55890,
            base + 6 * DAY_MS,
            false,
        )),
    );
    book
}

/// `--ui calendar`, `calendarmonth`, `calendarplan`, `calendarmeetup`,
/// `calendarnew`, `calendarsettings` and `reminder`: the demo book, and the
/// page. `chats` is whether any messenger screen is up, which shows the
/// "Coming up" card. Written unconditionally, like every scripted element.
pub fn seed_demo(cal: &mut Calendar, page: Option<&str>, chats: bool, reminder: bool) {
    cal.open = false;
    cal.reminder = None;
    if page.is_none() && !chats && !reminder {
        return;
    }
    let now = unix_ms();
    cal.me = "jeroen".into();
    cal.book = demo_book(now);
    cal.here = Some((53.2194, 6.5665));
    cal.page = Page::List;
    cal.day = Some(local_day(now));
    cal.month = None;
    let items = cal.book.items(now, now + 7 * DAY_MS, &cal.me, &offset_s);
    let pick = |meetup: bool| {
        items
            .iter()
            .find(|i| i.is_meetup() == meetup)
            .map(Item::pick)
    };
    if let Some(page) = page {
        cal.open = true;
        match page {
            "calendarmonth" => cal.page = Page::Month,
            "calendarplan" => {
                cal.page = Page::Detail;
                cal.picked = pick(false);
            }
            "calendarmeetup" => {
                cal.page = Page::Detail;
                cal.picked = pick(true);
            }
            "calendarnew" => {
                cal.page = Page::New;
                cal.new_chat = Some("dm:6d656574".into());
            }
            "calendarsettings" => cal.page = Page::Settings,
            _ => {}
        }
    }
    if reminder {
        if let Some(item) = items.iter().find(|i| i.is_meetup()).cloned() {
            cal.reminder = Some(Reminder {
                title: format!("Leave now for {}", item.title()),
                line: format!(
                    "Meet-up at {} · about 14 min walk",
                    crate::systems::chats::clock_time(item.start_ms)
                ),
                item,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::point::Meetup;

    const H: u64 = 3_600_000;
    /// Thursday 22 October 2026, 18:00Z.
    const T0: u64 = 1_792_692_000_000;

    fn utc(_: u64) -> i64 {
        0
    }

    fn plan(repeat: Option<Repeat>) -> Plan {
        Plan {
            id: "urn:uuid:p".into(),
            title: "Buurtvergadering".into(),
            start_ms: T0,
            end_ms: Some(T0 + 90 * 60_000),
            repeat,
            ..Plan::default()
        }
    }

    fn entry(channel: &str, sender: &str, at_ms: u64, json: String) -> Entry {
        Entry {
            channel: channel.into(),
            sender: sender.into(),
            at_ms,
            json,
        }
    }

    fn weekly() -> Option<Repeat> {
        Some(Repeat {
            every: Every::Week,
            interval: 1,
            until_ms: None,
        })
    }

    #[test]
    fn a_series_is_a_row_per_date_with_that_dates_answers() {
        let mut book = Book::default();
        let p = plan(weekly());
        book.add(entry(
            "g:1",
            "ann",
            1,
            MeetingObject::Plan(p.clone()).to_activity_json(),
        ));
        let answer = |rsvp, date| {
            MeetingObject::Answer {
                plan: p.id.clone(),
                rsvp,
                date,
            }
            .to_activity_json()
        };
        book.add(entry(
            "g:1",
            "jeroen",
            2,
            answer(Rsvp::Going, AnswerFor::Every),
        ));
        book.add(entry(
            "g:1",
            "jeroen",
            3,
            answer(Rsvp::No, AnswerFor::Date(T0 + 7 * 24 * H)),
        ));
        book.add(entry(
            "g:1",
            "bob",
            4,
            answer(Rsvp::Maybe, AnswerFor::First),
        ));
        // The planner skips the third date; somebody else's update is ignored.
        let skipped = Plan {
            skip: vec![T0 + 14 * 24 * H],
            ..p.clone()
        };
        book.add(entry(
            "g:1",
            "ann",
            5,
            MeetingObject::Update(skipped).to_activity_json(),
        ));
        book.add(entry(
            "g:1",
            "bob",
            6,
            MeetingObject::Update(Plan {
                title: "Hijacked".into(),
                ..p.clone()
            })
            .to_activity_json(),
        ));
        let items = book.items(T0 - H, T0 + 21 * 24 * H, "jeroen", &utc);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].mine, Some(Rsvp::Going));
        assert_eq!(items[0].maybe, vec!["bob".to_string()]);
        assert_eq!(items[1].mine, Some(Rsvp::No));
        assert!(
            items[1].maybe.is_empty(),
            "bob answered the first date only"
        );
        assert!(items[2].skipped);
        assert!(items.iter().all(|i| i.title() == "Buurtvergadering"));
        assert_eq!(items[0].author, "ann");
    }

    #[test]
    fn a_meetup_counts_who_is_going() {
        let mut book = Book::default();
        let place = Point {
            latitude: 53.2165,
            longitude: 6.5665,
            name: Some("Vismarkt".into()),
            meetup: Some(Meetup {
                start_ms: T0,
                going: false,
            }),
            ..Point::default()
        };
        let mut going = place.clone();
        going.meetup = Some(Meetup {
            start_ms: T0,
            going: true,
        });
        book.add(entry(
            "d:s",
            "sanne",
            1,
            place.to_activity_json(None).unwrap(),
        ));
        book.add(entry(
            "d:s",
            "jeroen",
            2,
            going.to_activity_json(None).unwrap(),
        ));
        let items = book.items(T0 - H, T0 + H, "jeroen", &utc);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title(), "Vismarkt");
        assert_eq!(
            items[0].going,
            vec!["sanne".to_string(), "jeroen".to_string()]
        );
        assert_eq!(items[0].mine, Some(Rsvp::Going));
        assert!(items[0].is_meetup());
    }

    #[test]
    fn a_line_is_kept_once_and_old_plans_are_dropped_with_their_answers() {
        let mut book = Book::default();
        let p = plan(None);
        let e = entry(
            "g:1",
            "ann",
            1,
            MeetingObject::Plan(p.clone()).to_activity_json(),
        );
        assert!(book.add(e.clone()));
        assert!(!book.add(e));
        book.add(entry(
            "g:1",
            "bob",
            2,
            MeetingObject::Answer {
                plan: p.id.clone(),
                rsvp: Rsvp::Going,
                date: AnswerFor::First,
            }
            .to_activity_json(),
        ));
        // A series with no end is never over.
        let series = Plan {
            id: "urn:uuid:s".into(),
            ..plan(weekly())
        };
        book.add(entry(
            "g:1",
            "ann",
            3,
            MeetingObject::Plan(series).to_activity_json(),
        ));
        book.prune(T0 + 10 * 24 * H, &utc);
        assert_eq!(book.entries.len(), 3, "ten days on, all kept");
        book.prune(T0 + 40 * 24 * H, &utc);
        assert_eq!(
            book.entries.len(),
            1,
            "forty days on, only the endless series"
        );
    }

    #[test]
    fn the_book_reads_back_as_saved() {
        let mut book = Book::default();
        book.prefs.answered_only.push("x".into());
        book.add(entry(
            "g:1",
            "ann",
            1,
            MeetingObject::Plan(plan(None)).to_activity_json(),
        ));
        let bytes = serde_json::to_vec(&book).unwrap();
        assert_eq!(serde_json::from_slice::<Book>(&bytes).unwrap(), book);
        // An older or newer file with fields missing still reads.
        let bare: Book = serde_json::from_str("{}").unwrap();
        assert_eq!(bare, Book::default());
    }

    #[test]
    fn store_keys_cannot_climb_out_of_their_prefix() {
        let k = store_key("../../etc", "a/b");
        assert!(k.starts_with(STORE_PREFIX));
        assert!(!k[STORE_PREFIX.len()..].contains(".."), "{k}");
        assert_eq!(k.matches('/').count(), 2, "{k}");
    }

    #[test]
    fn a_rule_reads_as_words() {
        let p = plan(Some(Repeat {
            every: Every::Month,
            interval: 1,
            until_ms: None,
        }));
        let words = rule_words(&p).unwrap();
        assert!(words.starts_with("Every month on the 4th "), "{words}");
        assert_eq!(rule_words(&plan(None)), None);
    }
}
