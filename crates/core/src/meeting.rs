//! What a meeting leaves in its chat: a plan with its answers, polls and their
//! votes, notes taken during the call, and the record afterwards.
//!
//! Each is a sealed chat message, an ActivityStreams object in
//! `Payload::Object` beside [`crate::point`] (`docs/notes/calls.md`, "What a
//! meeting leaves in the chat"), so the server sees none of it. Standard
//! shapes where ActivityStreams has one — a plan is an `Event`, a poll a
//! `Question` whose options are `oneOf`/`anyOf`, an answer an `Accept`,
//! `TentativeAccept` or `Reject` — and fields under our own `cartopolis:`
//! prefix where it has none.
//!
//! **An answer names what it answers by the object's `id`**, a `urn:uuid:` the
//! author picked, never by the chat's log position: the app keeps no log
//! positions for its lines, and an id survives a history handed to somebody
//! who joined late. Everybody counts the answers the same way: per person,
//! the newest wins.
//!
//! A meet-up (a place and a time) stays a [`crate::point::Point`]: it has a
//! `location`, which none of these do, and that is how the two are told apart.
//!
//! # Plans that repeat
//!
//! A plan may carry a rule — every week, every other week, or every month on
//! the same weekday ("the 4th Thursday") — with an optional last date, and a
//! list of dates taken out (`docs/mockups/calendar/v2-repeat`). It stays one
//! plan: every reader works out the dates from the rule ([`Plan::dates`]), so a
//! build that predates the rule sees the first date as an ordinary plan.
//!
//! **The dates keep the first date's local clock time** in the time zone of
//! the device reading them, so a meeting at 20:00 stays at 20:00 across the
//! change to summer time. A person reading from another time zone sees the
//! series at the planner's clock time in theirs; the plan carries no zone.
//!
//! An answer names the date it is for (`cartopolis:date`), or every date
//! (`cartopolis:every`); one with neither, which is every answer an older build
//! sends, is for the first date. Skipping a date or ending the series is an
//! `Update` carrying the whole plan again, believed only from the plan's own
//! author ([`current_plan`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::point::{civil_from_days, days_from_civil, parse_rfc3339, rfc3339_utc};

/// Longest title (a plan's, a poll's question), in characters.
pub const MAX_TITLE: usize = 140;
/// Longest note, description or option, in characters.
pub const MAX_TEXT: usize = 2000;
/// Most options in a poll, items on an agenda.
pub const MAX_ITEMS: usize = 12;
/// Most lines of transcript a record carries: an hour of talk, give or take.
pub const MAX_TRANSCRIPT: usize = 1500;

/// A meeting planned in a chat: a call at a time, with an agenda.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    pub id: String,
    pub title: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub text: Option<String>,
    pub agenda: Vec<AgendaItem>,
    /// How it comes back, if it does.
    pub repeat: Option<Repeat>,
    /// Dates of the series taken out, as the start of each (Unix ms, UTC).
    pub skip: Vec<u64>,
}

/// Most dates a plan can have taken out.
pub const MAX_SKIP: usize = 100;

/// How often a repeating plan comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Every {
    /// The same weekday, every `interval` weeks.
    Week,
    /// The same weekday of the month ("the 4th Thursday"), every `interval`
    /// months. A month without that weekday (a 5th Thursday) is passed over.
    Month,
}

/// A plan's rule for coming back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Repeat {
    pub every: Every,
    /// Every how many weeks or months: 1 to 4.
    pub interval: u32,
    /// The start of the last date, if the series ends. Inclusive.
    pub until_ms: Option<u64>,
}

/// Which of a plan's dates an answer is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnswerFor {
    /// The plan as a whole, which for a repeating plan is its first date:
    /// what an answer naming no date means, and all an older build sends.
    #[default]
    First,
    /// The date starting at this Unix millisecond.
    Date(u64),
    /// Every date of the series.
    Every,
}

const DAY_S: i64 = 86_400;
/// How far [`Plan::dates`] looks before giving up on a rule.
const MAX_STEPS: u32 = 2000;

impl Plan {
    /// How long each date lasts: its end, else its agenda's minutes, else an
    /// hour.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        if let Some(end) = self.end_ms.filter(|e| *e > self.start_ms) {
            return end - self.start_ms;
        }
        let agenda: u64 = self.agenda.iter().map(|i| u64::from(i.minutes)).sum();
        if agenda > 0 {
            agenda * 60_000
        } else {
            60 * 60_000
        }
    }

    /// Whether this date was taken out of the series.
    #[must_use]
    pub fn is_skipped(&self, date_ms: u64) -> bool {
        self.skip.contains(&date_ms)
    }

    /// The starts of its dates in `from_ms..to_ms`, skipped ones included
    /// (they are shown, struck through). `offset_s` is this device's UTC
    /// offset in seconds at an instant: the dates keep the first date's local
    /// clock time across a change of offset.
    pub fn dates(&self, from_ms: u64, to_ms: u64, offset_s: &dyn Fn(u64) -> i64) -> Vec<u64> {
        let Some(rule) = self.repeat else {
            return if (from_ms..to_ms).contains(&self.start_ms) {
                vec![self.start_ms]
            } else {
                Vec::new()
            };
        };
        let interval = i64::from(rule.interval.clamp(1, 4));
        let off0 = offset_s(self.start_ms);
        let local0 = (self.start_ms / 1000) as i64 + off0;
        let (day0, clock) = (local0.div_euclid(DAY_S), local0.rem_euclid(DAY_S));
        // Local midnight-based seconds back to UTC: guess with the first
        // date's offset, then use the offset at the guess.
        let to_utc = |day: i64| -> Option<u64> {
            let local = day * DAY_S + clock;
            let guess = local - off0;
            let utc = local - offset_s(u64::try_from(guess).ok()? * 1000);
            u64::try_from(utc)
                .ok()
                .map(|s| s * 1000 + self.start_ms % 1000)
        };
        let (y0, m0, d0) = civil_from_days(day0);
        let weekday = |day: i64| (day + 4).rem_euclid(7); // 1970-01-01 was a Thursday
        let nth = (d0 - 1) / 7;
        let mut out = Vec::new();
        for k in 0..i64::from(MAX_STEPS) {
            let day = match rule.every {
                Every::Week => Some(day0 + k * 7 * interval),
                Every::Month => {
                    let mi = y0 * 12 + (m0 - 1) + k * interval;
                    let (y, m) = (mi.div_euclid(12), mi.rem_euclid(12) + 1);
                    let first = days_from_civil(y, m, 1);
                    let day = first + (weekday(day0) - weekday(first)).rem_euclid(7) + 7 * nth;
                    (civil_from_days(day).1 == m).then_some(day)
                }
            };
            let Some(at) = day.and_then(to_utc) else {
                continue;
            };
            if at >= to_ms || rule.until_ms.is_some_and(|u| at > u) {
                break;
            }
            if at >= from_ms {
                out.push(at);
            }
        }
        out
    }

    /// The next date that has not ended by `now_ms` and is not skipped.
    pub fn next_date(&self, now_ms: u64, offset_s: &dyn Fn(u64) -> i64) -> Option<u64> {
        let from = now_ms.saturating_sub(self.duration_ms());
        let horizon = now_ms + 400 * DAY_S as u64 * 1000;
        self.dates(from, horizon, offset_s)
            .into_iter()
            .find(|d| d + self.duration_ms() > now_ms && !self.is_skipped(*d))
    }

    /// How many dates the series has, when it ends.
    pub fn date_count(&self, offset_s: &dyn Fn(u64) -> i64) -> Option<usize> {
        let until = match self.repeat {
            None => return Some(1),
            Some(r) => r.until_ms?,
        };
        Some(self.dates(self.start_ms, until + 1, offset_s).len())
    }
}

/// One item of an agenda, and the minutes it was given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgendaItem {
    pub name: String,
    #[serde(default)]
    pub minutes: u32,
}

impl AgendaItem {
    /// "Budget 10", "Budget · 10 min" or "- Budget" → an item, as the web
    /// chat's `agendaLine` reads it: a number at the end is its minutes.
    #[must_use]
    pub fn from_line(line: &str) -> Option<Self> {
        let t = line.trim();
        let t = t.strip_prefix(['-', '*', '•']).map_or(t, str::trim_start);
        if t.is_empty() {
            return None;
        }
        let plain = || AgendaItem {
            name: t.to_string(),
            minutes: 0,
        };
        let lower = t.to_ascii_lowercase();
        let unit = ["minutes", "mins", "min", "m"]
            .into_iter()
            .find(|u| lower.ends_with(u))
            .map_or(0, str::len);
        let rest = t[..t.len() - unit].trim_end();
        let digits = rest.len() - rest.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        if !(1..=3).contains(&digits) {
            return Some(plain());
        }
        let before = &rest[..rest.len() - digits];
        let sep = |c: char| c.is_whitespace() || matches!(c, '·' | ',' | ':' | '-');
        let name = before.trim_end_matches(sep);
        if name.len() == before.len() || name.trim().is_empty() {
            return Some(plain());
        }
        Some(AgendaItem {
            name: name.trim().to_string(),
            minutes: rest[rest.len() - digits..].parse().unwrap_or(0),
        })
    }
}

/// Whether somebody comes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rsvp {
    Going,
    Maybe,
    No,
}

impl Rsvp {
    const fn kind(self) -> &'static str {
        match self {
            Rsvp::Going => "Accept",
            Rsvp::Maybe => "TentativeAccept",
            Rsvp::No => "Reject",
        }
    }

    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Rsvp::Going => "Going",
            Rsvp::Maybe => "Maybe",
            Rsvp::No => "Can't come",
        }
    }
}

/// A question with options; `multiple` lets a person choose more than one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Poll {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
    pub multiple: bool,
}

/// What kind of note: said, decided, or somebody's to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    Note,
    Decision,
    Action,
}

impl NoteKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            NoteKind::Note => "Note",
            NoteKind::Decision => "Decision",
            NoteKind::Action => "Action",
        }
    }
}

/// What a meeting came to, posted when it ends.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub title: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Who was in it, by username.
    #[serde(default)]
    pub people: Vec<String>,
    #[serde(default)]
    pub notes: Vec<RecordNote>,
    #[serde(default)]
    pub polls: Vec<RecordPoll>,
    /// (who, what): finished lines of captions, if anybody had them on.
    #[serde(default)]
    pub transcript: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordNote {
    pub kind: NoteKind,
    pub text: String,
    #[serde(default)]
    pub by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordPoll {
    pub question: String,
    /// Each option and how many chose it.
    pub results: Vec<(String, u32)>,
}

/// Any of the objects a meeting leaves in a chat.
#[derive(Debug, Clone, PartialEq)]
pub enum MeetingObject {
    Plan(Plan),
    Answer {
        plan: String,
        rsvp: Rsvp,
        date: AnswerFor,
    },
    /// The plan again, changed by its author: a date skipped, the series
    /// ended. Only the author's counts ([`current_plan`]).
    Update(Plan),
    Poll(Poll),
    /// Somebody's choice in a poll; an empty one takes their vote back.
    Vote {
        poll: String,
        choices: Vec<String>,
    },
    Note {
        kind: NoteKind,
        text: String,
    },
    Record(Record),
}

const CONTEXT: &str = "https://www.w3.org/ns/activitystreams";

fn clip(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

/// A fresh id for a plan or a poll.
#[must_use]
pub fn new_id() -> String {
    format!("urn:uuid:{}", uuid::Uuid::new_v4())
}

#[derive(Serialize, Deserialize)]
struct RepeatJson {
    every: Every,
    #[serde(default = "one")]
    interval: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    until: Option<String>,
}

const fn one() -> u32 {
    1
}

/// A plan as its `Event`, without the `@context` (an `Update` carries it
/// inside one that has it).
fn plan_json(p: &Plan) -> Value {
    let mut v = serde_json::json!({
        "type": "Event",
        "id": p.id,
        "name": clip(&p.title, MAX_TITLE),
        "startTime": rfc3339_utc(p.start_ms),
        "endTime": p.end_ms.map(rfc3339_utc),
        "content": p.text.as_deref().map(|t| clip(t, MAX_TEXT)).filter(|t| !t.is_empty()),
        "cartopolis:call": true,
        "cartopolis:agenda": p.agenda.iter().take(MAX_ITEMS).map(|i| AgendaItem {
            name: clip(&i.name, MAX_TITLE),
            minutes: i.minutes.min(600),
        }).collect::<Vec<_>>(),
    });
    if let Some(r) = p.repeat {
        v["cartopolis:repeat"] = serde_json::json!(RepeatJson {
            every: r.every,
            interval: r.interval.clamp(1, 4),
            until: r.until_ms.map(rfc3339_utc),
        });
        if !p.skip.is_empty() {
            v["cartopolis:skip"] = p
                .skip
                .iter()
                .take(MAX_SKIP)
                .map(|ms| rfc3339_utc(*ms))
                .collect::<Vec<_>>()
                .into();
        }
    }
    v
}

/// The plan in an `Event`; `None` for an event that is not a call's plan.
fn plan_from(v: &Value) -> Option<Plan> {
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    let time = |k: &str| s(k).and_then(parse_rfc3339);
    if s("type")? != "Event" || v.get("cartopolis:call").is_none() || v.get("location").is_some() {
        return None;
    }
    let repeat = v
        .get("cartopolis:repeat")
        .and_then(|r| serde_json::from_value::<RepeatJson>(r.clone()).ok())
        .map(|r| Repeat {
            every: r.every,
            interval: r.interval.clamp(1, 4),
            until_ms: r.until.as_deref().and_then(parse_rfc3339),
        });
    let skip = if repeat.is_some() {
        v.get("cartopolis:skip")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter_map(parse_rfc3339)
                    .take(MAX_SKIP)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Some(Plan {
        id: s("id")?.to_string(),
        title: clip(s("name").unwrap_or(""), MAX_TITLE),
        start_ms: time("startTime")?,
        end_ms: time("endTime"),
        text: s("content").map(|t| clip(t, MAX_TEXT)),
        agenda: v
            .get("cartopolis:agenda")
            .and_then(|a| serde_json::from_value::<Vec<AgendaItem>>(a.clone()).ok())
            .unwrap_or_default()
            .into_iter()
            .take(MAX_ITEMS)
            .collect(),
        repeat,
        skip,
    })
}

#[derive(Serialize, Deserialize)]
struct OptionJson {
    #[serde(rename = "type")]
    kind: String,
    name: String,
}

impl MeetingObject {
    /// The ActivityStreams JSON it travels as.
    #[must_use]
    pub fn to_activity_json(&self) -> String {
        let v = match self {
            MeetingObject::Plan(p) => {
                let mut v = plan_json(p);
                v["@context"] = CONTEXT.into();
                v
            }
            MeetingObject::Update(p) => serde_json::json!({
                "@context": CONTEXT,
                "type": "Update",
                "object": plan_json(p),
            }),
            MeetingObject::Answer { plan, rsvp, date } => {
                let mut v = serde_json::json!({
                    "@context": CONTEXT,
                    "type": rsvp.kind(),
                    "object": plan,
                });
                match date {
                    AnswerFor::First => {}
                    AnswerFor::Date(ms) => v["cartopolis:date"] = rfc3339_utc(*ms).into(),
                    AnswerFor::Every => v["cartopolis:every"] = true.into(),
                }
                v
            }
            MeetingObject::Poll(p) => {
                let options: Vec<OptionJson> = p
                    .options
                    .iter()
                    .map(|o| clip(o, MAX_TITLE))
                    .filter(|o| !o.is_empty())
                    .take(MAX_ITEMS)
                    .map(|name| OptionJson {
                        kind: "Note".into(),
                        name,
                    })
                    .collect();
                let mut v = serde_json::json!({
                    "@context": CONTEXT,
                    "type": "Question",
                    "id": p.id,
                    "name": clip(&p.question, MAX_TITLE),
                });
                v[if p.multiple { "anyOf" } else { "oneOf" }] = serde_json::json!(options);
                v
            }
            MeetingObject::Vote { poll, choices } => serde_json::json!({
                "@context": CONTEXT,
                "type": "Note",
                "inReplyTo": poll,
                "cartopolis:choices": choices.iter().take(MAX_ITEMS).map(|c| clip(c, MAX_TITLE)).collect::<Vec<_>>(),
            }),
            MeetingObject::Note { kind, text } => serde_json::json!({
                "@context": CONTEXT,
                "type": "Note",
                "content": clip(text, MAX_TEXT),
                "cartopolis:meeting": kind,
            }),
            MeetingObject::Record(r) => {
                let mut r = r.clone();
                r.transcript.truncate(MAX_TRANSCRIPT);
                serde_json::json!({
                    "@context": CONTEXT,
                    "type": "Article",
                    "name": if r.title.is_empty() { "Meeting record".to_string() } else { clip(&r.title, MAX_TITLE) },
                    "startTime": rfc3339_utc(r.start_ms),
                    "endTime": rfc3339_utc(r.end_ms),
                    "content": record_text(&r),
                    "cartopolis:record": r,
                })
            }
        };
        v.to_string()
    }

    /// Read one back; `None` for anything else (a point, an object from a
    /// newer build).
    #[must_use]
    pub fn from_activity_json(json: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(json).ok()?;
        if v.get("location").is_some() {
            return None;
        }
        let s = |k: &str| v.get(k).and_then(Value::as_str);
        let time = |k: &str| s(k).and_then(parse_rfc3339);
        Some(match s("type")? {
            "Event" => MeetingObject::Plan(plan_from(&v)?),
            "Update" => MeetingObject::Update(plan_from(v.get("object")?)?),
            kind @ ("Accept" | "TentativeAccept" | "Reject") => MeetingObject::Answer {
                plan: s("object")?.to_string(),
                rsvp: match kind {
                    "Accept" => Rsvp::Going,
                    "TentativeAccept" => Rsvp::Maybe,
                    _ => Rsvp::No,
                },
                date: if v.get("cartopolis:every").and_then(Value::as_bool) == Some(true) {
                    AnswerFor::Every
                } else {
                    time("cartopolis:date").map_or(AnswerFor::First, AnswerFor::Date)
                },
            },
            "Question" => {
                let (list, multiple) = match (v.get("oneOf"), v.get("anyOf")) {
                    (_, Some(a)) => (a, true),
                    (Some(o), None) => (o, false),
                    (None, None) => return None,
                };
                let options: Vec<String> = serde_json::from_value::<Vec<OptionJson>>(list.clone())
                    .ok()?
                    .into_iter()
                    .map(|o| clip(&o.name, MAX_TITLE))
                    .take(MAX_ITEMS)
                    .collect();
                MeetingObject::Poll(Poll {
                    id: s("id")?.to_string(),
                    question: clip(s("name").unwrap_or(""), MAX_TITLE),
                    options,
                    multiple,
                })
            }
            "Note" if v.get("inReplyTo").is_some() && v.get("cartopolis:choices").is_some() => {
                MeetingObject::Vote {
                    poll: s("inReplyTo")?.to_string(),
                    choices: serde_json::from_value::<Vec<String>>(v["cartopolis:choices"].clone())
                        .ok()?
                        .into_iter()
                        .take(MAX_ITEMS)
                        .collect(),
                }
            }
            "Note" if v.get("cartopolis:meeting").is_some() => MeetingObject::Note {
                kind: serde_json::from_value(v["cartopolis:meeting"].clone()).ok()?,
                text: clip(s("content").unwrap_or(""), MAX_TEXT),
            },
            "Article" if v.get("cartopolis:record").is_some() => {
                let mut r: Record = serde_json::from_value(v["cartopolis:record"].clone()).ok()?;
                r.transcript.truncate(MAX_TRANSCRIPT);
                MeetingObject::Record(r)
            }
            _ => return None,
        })
    }

    /// A line of text for a reader that shows only text: a notification, a
    /// chat preview, an app that predates the kind.
    #[must_use]
    pub fn caption(&self) -> String {
        match self {
            MeetingObject::Plan(p) => format!("📅 {}", p.title),
            MeetingObject::Update(p) => format!("📅 {} changed", p.title),
            MeetingObject::Answer { rsvp, .. } => rsvp.words().to_string(),
            MeetingObject::Poll(p) => format!("📊 {}", p.question),
            MeetingObject::Vote { choices, .. } if choices.is_empty() => "took a vote back".into(),
            MeetingObject::Vote { choices, .. } => format!("voted {}", choices.join(", ")),
            MeetingObject::Note { kind, text } => format!("{}: {text}", kind.label()),
            MeetingObject::Record(r) => {
                let mins = r.end_ms.saturating_sub(r.start_ms) / 60_000;
                format!("Meeting record · {mins} min · {} people", r.people.len())
            }
        }
    }

    /// Whether the chat shows it as a line of its own. Answers and votes are
    /// counted on what they answer instead.
    #[must_use]
    pub const fn is_line(&self) -> bool {
        !matches!(
            self,
            MeetingObject::Answer { .. } | MeetingObject::Vote { .. }
        )
    }
}

/// A record as plain text: what an app that cannot draw it shows, and what is
/// copied out of it.
#[must_use]
pub fn record_text(r: &Record) -> String {
    let mut out = String::new();
    let mins = r.end_ms.saturating_sub(r.start_ms) / 60_000;
    out.push_str(&format!("{mins} min with {}\n", r.people.join(", ")));
    for kind in [NoteKind::Decision, NoteKind::Action, NoteKind::Note] {
        let notes: Vec<&RecordNote> = r.notes.iter().filter(|n| n.kind == kind).collect();
        if notes.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{}s\n", kind.label()));
        for n in notes {
            out.push_str(&format!("- {}\n", n.text));
        }
    }
    for p in &r.polls {
        out.push_str(&format!("\n{}\n", p.question));
        for (o, n) in &p.results {
            out.push_str(&format!("- {o}: {n}\n"));
        }
    }
    out
}

/// Counted answers for the date of `plan` starting at `date_ms`: per person
/// the newest wins, so changing one's mind is sending again, and an answer for
/// every date counts as much as one for this date — whichever came last.
/// `answers` are (who, object) in the order they were sent.
#[must_use]
pub fn tally_rsvps<'a>(
    plan: &Plan,
    date_ms: u64,
    answers: impl IntoIterator<Item = (&'a str, &'a MeetingObject)>,
) -> Vec<(String, Rsvp)> {
    let mut out: Vec<(String, Rsvp)> = Vec::new();
    for (who, o) in answers {
        if let MeetingObject::Answer {
            plan: p,
            rsvp,
            date,
        } = o
        {
            let this = match date {
                AnswerFor::First => date_ms == plan.start_ms,
                AnswerFor::Date(d) => *d == date_ms,
                AnswerFor::Every => true,
            };
            if *p == plan.id && this {
                out.retain(|(w, _)| w != who);
                out.push((who.to_string(), *rsvp));
            }
        }
    }
    out
}

/// The plan as its author last left it: the newest [`MeetingObject::Update`]
/// for it sent by `author`, else the plan itself. An update from anybody else
/// is not believed, and neither is one that changes the id.
#[must_use]
pub fn current_plan<'a>(
    plan: &Plan,
    author: &str,
    said: impl IntoIterator<Item = (&'a str, &'a MeetingObject)>,
) -> Plan {
    let mut out = plan.clone();
    for (who, o) in said {
        if let MeetingObject::Update(p) = o {
            if who == author && p.id == plan.id {
                out = p.clone();
            }
        }
    }
    out
}

/// What an update changed, for the line it leaves in the chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanChange {
    /// These dates were taken out.
    Skipped(Vec<u64>),
    /// These dates were put back.
    Restored(Vec<u64>),
    /// The series now ends with the date starting then.
    Ends(u64),
    /// The series no longer ends.
    NoEnd,
    /// Something else: the time, the title, the agenda.
    Other,
}

impl PlanChange {
    /// What `after` changed in `before`.
    #[must_use]
    pub fn between(before: &Plan, after: &Plan) -> Self {
        let skipped: Vec<u64> = after
            .skip
            .iter()
            .filter(|d| !before.skip.contains(d))
            .copied()
            .collect();
        let restored: Vec<u64> = before
            .skip
            .iter()
            .filter(|d| !after.skip.contains(d))
            .copied()
            .collect();
        let until = |p: &Plan| p.repeat.and_then(|r| r.until_ms);
        let rest_same = Plan {
            skip: Vec::new(),
            repeat: after.repeat.map(|r| Repeat {
                until_ms: None,
                ..r
            }),
            ..after.clone()
        } == Plan {
            skip: Vec::new(),
            repeat: before.repeat.map(|r| Repeat {
                until_ms: None,
                ..r
            }),
            ..before.clone()
        };
        if !rest_same {
            Self::Other
        } else if !skipped.is_empty() && restored.is_empty() && until(before) == until(after) {
            Self::Skipped(skipped)
        } else if skipped.is_empty() && !restored.is_empty() && until(before) == until(after) {
            Self::Restored(restored)
        } else if skipped.is_empty() && restored.is_empty() && until(before) != until(after) {
            until(after).map_or(Self::NoEnd, Self::Ends)
        } else {
            Self::Other
        }
    }

    /// In words, with `date` naming a date ("Thursday 29 October").
    #[must_use]
    pub fn words(&self, title: &str, date: &dyn Fn(u64) -> String) -> String {
        let list = |ds: &[u64]| ds.iter().map(|d| date(*d)).collect::<Vec<_>>().join(", ");
        match self {
            Self::Skipped(ds) => format!("skipped {title} on {}", list(ds)),
            Self::Restored(ds) => format!("put {title} back on {}", list(ds)),
            Self::Ends(d) => format!("ended {title} after {}", date(*d)),
            Self::NoEnd => format!("made {title} repeat with no end"),
            Self::Other => format!("changed {title}"),
        }
    }
}

/// A poll's votes: per option, who chose it. Per person the newest vote wins;
/// a choice that is not one of the options is not counted.
#[must_use]
pub fn tally_votes<'a>(
    poll: &Poll,
    votes: impl IntoIterator<Item = (&'a str, &'a MeetingObject)>,
) -> Vec<(String, Vec<String>)> {
    let mut latest: Vec<(String, Vec<String>)> = Vec::new();
    for (who, o) in votes {
        if let MeetingObject::Vote { poll: p, choices } = o {
            if *p == poll.id {
                latest.retain(|(w, _)| w != who);
                let mut c: Vec<String> = choices
                    .iter()
                    .filter(|c| poll.options.contains(c))
                    .cloned()
                    .collect();
                if !poll.multiple {
                    c.truncate(1);
                }
                latest.push((who.to_string(), c));
            }
        }
    }
    poll.options
        .iter()
        .map(|o| {
            let who = latest
                .iter()
                .filter(|(_, c)| c.contains(o))
                .map(|(w, _)| w.clone())
                .collect();
            (o.clone(), who)
        })
        .collect()
}

/// What a meeting from `start_ms` to `end_ms` came to, from what was said in
/// its chat as `(who, sent at ms, object)`: the notes and polls posted while it
/// ran (a minute's grace before), the people, and the transcript. The web's
/// `buildRecord` does the same. Titled after the plan it belongs to, if any.
#[must_use]
pub fn build_record(
    said: &[(String, u64, &MeetingObject)],
    start_ms: u64,
    end_ms: u64,
    mut people: Vec<String>,
    mut transcript: Vec<(String, String)>,
    chat_name: &str,
) -> Record {
    let during = |at: u64| at + 60_000 >= start_ms && at <= end_ms;
    let mut notes = Vec::new();
    let mut polls = Vec::new();
    let mut plan: Option<&Plan> = None;
    for (who, at, o) in said {
        match o {
            MeetingObject::Note { kind, text } if during(*at) => notes.push(RecordNote {
                kind: *kind,
                text: text.clone(),
                by: who.clone(),
            }),
            MeetingObject::Poll(p) if during(*at) => polls.push(RecordPoll {
                question: p.question.clone(),
                results: tally_votes(p, said.iter().map(|(w, _, o)| (w.as_str(), *o)))
                    .into_iter()
                    .map(|(o, who)| (o, who.len() as u32))
                    .collect(),
            }),
            // The plan this meeting is: one starting up to 15 minutes after
            // it began or up to three hours before, the nearest.
            MeetingObject::Plan(p)
                if p.start_ms <= start_ms + 15 * 60_000
                    && start_ms <= p.start_ms + 3 * 3_600_000 =>
            {
                if plan
                    .is_none_or(|q| p.start_ms.abs_diff(start_ms) < q.start_ms.abs_diff(start_ms))
                {
                    plan = Some(p);
                }
            }
            _ => {}
        }
    }
    people.sort();
    people.dedup();
    let excess = transcript.len().saturating_sub(MAX_TRANSCRIPT);
    transcript.drain(..excess);
    Record {
        title: plan.map_or_else(
            || {
                if chat_name.is_empty() {
                    "Meeting record".to_string()
                } else {
                    format!("Call in {chat_name}")
                }
            },
            |p| p.title.clone(),
        ),
        start_ms,
        end_ms,
        people,
        notes,
        polls,
        transcript,
    }
}

impl Record {
    /// Whether it says anything a chat needs: a meeting that left no note,
    /// poll or transcript posts no record.
    #[must_use]
    pub fn has_content(&self) -> bool {
        !(self.notes.is_empty() && self.polls.is_empty() && self.transcript.is_empty())
    }
}

/// A plan as an iCalendar file, for a person's own calendar. A one-off plan is
/// written in UTC, local times being the calendar's business. A repeating one
/// with `zone` (an IANA name and the offset function [`Plan::dates`] takes) is
/// written in that zone's local time, so the calendar repeats it at the same
/// clock time across a change to summer time, as this app does.
#[must_use]
pub fn ics(plan: &Plan, chat: &str, zone: Option<(&str, &dyn Fn(u64) -> i64)>) -> String {
    let stamp = |ms: u64| rfc3339_utc(ms).replace(['-', ':'], "");
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace(';', "\\;")
            .replace(',', "\\,")
            .replace('\n', "\\n")
    };
    let zone = zone.filter(|_| plan.repeat.is_some());
    // `:20261014T180000Z`, or `;TZID=Europe/Amsterdam:20261014T200000`.
    let when = |ms: u64| match zone {
        Some((name, offset)) => {
            let local = (ms as i64 / 1000 + offset(ms)).max(0) as u64 * 1000;
            format!(";TZID={name}:{}", stamp(local).trim_end_matches('Z'))
        }
        None => format!(":{}", stamp(ms)),
    };
    let end = plan.start_ms + plan.duration_ms();
    let mut desc = format!("A call in {chat} on Cartopolis.");
    if let Some(t) = &plan.text {
        desc.push_str(&format!("\n\n{t}"));
    }
    if !plan.agenda.is_empty() {
        desc.push_str("\n\nAgenda:");
        for i in &plan.agenda {
            desc.push_str(&format!("\n- {}", i.name));
            if i.minutes > 0 {
                desc.push_str(&format!(" ({} min)", i.minutes));
            }
        }
    }
    let mut rule = String::new();
    if let Some(r) = plan.repeat {
        let offset = zone.map_or(0, |(_, o)| o(plan.start_ms));
        let day = (plan.start_ms as i64 / 1000 + offset).div_euclid(DAY_S);
        let weekday = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"][(day + 4).rem_euclid(7) as usize];
        let nth = (civil_from_days(day).2 - 1) / 7 + 1;
        rule.push_str(&match r.every {
            Every::Week => format!("RRULE:FREQ=WEEKLY;INTERVAL={}", r.interval.clamp(1, 4)),
            Every::Month => format!(
                "RRULE:FREQ=MONTHLY;INTERVAL={};BYDAY={nth}{weekday}",
                r.interval.clamp(1, 4)
            ),
        });
        if let Some(u) = r.until_ms {
            rule.push_str(&format!(";UNTIL={}", stamp(u)));
        }
        rule.push_str("\r\n");
        for d in &plan.skip {
            rule.push_str(&format!("EXDATE{}\r\n", when(*d)));
        }
    }
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Cartopolis//Meetings//EN\r\nBEGIN:VEVENT\r\nUID:{}@cartopolis\r\nDTSTAMP:{}\r\nDTSTART{}\r\nDTEND{}\r\n{rule}SUMMARY:{}\r\nDESCRIPTION:{}\r\nLOCATION:{}\r\nBEGIN:VALARM\r\nTRIGGER:-PT10M\r\nACTION:DISPLAY\r\nDESCRIPTION:{}\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        plan.id.trim_start_matches("urn:uuid:"),
        stamp(plan.start_ms),
        when(plan.start_ms),
        when(end),
        esc(&plan.title),
        esc(&desc),
        esc(&format!("Cartopolis · {chat}")),
        esc(&plan.title),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::Point;

    fn plan() -> Plan {
        Plan {
            id: "urn:uuid:1".into(),
            title: "Q4 planning".into(),
            start_ms: 1_792_000_800_000,
            end_ms: Some(1_792_004_400_000),
            text: Some("Bring the numbers".into()),
            agenda: vec![AgendaItem {
                name: "Budget".into(),
                minutes: 10,
            }],
            repeat: None,
            skip: Vec::new(),
        }
    }

    #[test]
    fn every_kind_reads_back_as_written() {
        let all = [
            MeetingObject::Plan(plan()),
            MeetingObject::Answer {
                plan: "urn:uuid:1".into(),
                rsvp: Rsvp::Maybe,
                date: AnswerFor::First,
            },
            MeetingObject::Poll(Poll {
                id: "urn:uuid:2".into(),
                question: "Which day?".into(),
                options: vec!["Fri".into(), "Sat".into()],
                multiple: true,
            }),
            MeetingObject::Vote {
                poll: "urn:uuid:2".into(),
                choices: vec!["Sat".into()],
            },
            MeetingObject::Note {
                kind: NoteKind::Action,
                text: "Ann books the stage".into(),
            },
            MeetingObject::Record(Record {
                title: "Q4 planning".into(),
                start_ms: 1_792_000_800_000,
                end_ms: 1_792_002_600_000,
                people: vec!["ann".into(), "bob".into()],
                notes: vec![RecordNote {
                    kind: NoteKind::Decision,
                    text: "Gazebos stay".into(),
                    by: "ann".into(),
                }],
                polls: vec![RecordPoll {
                    question: "Which day?".into(),
                    results: vec![("Sat".into(), 2)],
                }],
                transcript: vec![("bob".into(), "hello".into())],
            }),
        ];
        for o in all {
            let json = o.to_activity_json();
            assert_eq!(MeetingObject::from_activity_json(&json), Some(o), "{json}");
        }
    }

    /// The vectors the web's reader (`crates/atlas/web/src/lib/meeting.ts`)
    /// is tested against too: what one writes, the other reads.
    #[test]
    fn the_shared_vectors_read_as_their_kind() {
        let vectors: Vec<Value> =
            serde_json::from_str(include_str!("../testdata/meeting-vectors.json")).unwrap();
        for v in vectors {
            let read = MeetingObject::from_activity_json(&v["json"].to_string());
            let kind = read.as_ref().map(|o| match o {
                MeetingObject::Plan(_) => "plan",
                MeetingObject::Update(_) => "update",
                MeetingObject::Answer { .. } => "answer",
                MeetingObject::Poll(_) => "poll",
                MeetingObject::Vote { .. } => "vote",
                MeetingObject::Note { .. } => "note",
                MeetingObject::Record(_) => "record",
            });
            assert_eq!(kind, v["kind"].as_str(), "{}", v["json"]);
            // And what this side writes reads back the same.
            if let Some(o) = read {
                assert_eq!(
                    MeetingObject::from_activity_json(&o.to_activity_json()),
                    Some(o)
                );
            }
        }
    }

    #[test]
    fn a_meetup_is_a_point_and_never_a_plan() {
        let p = Point {
            latitude: 53.2,
            longitude: 6.56,
            meetup: Some(crate::point::Meetup {
                start_ms: 1_792_000_800_000,
                going: false,
            }),
            ..Point::default()
        };
        let json = p.to_activity_json(None).unwrap();
        assert_eq!(MeetingObject::from_activity_json(&json), None);
        // And a plan is never read as a point.
        let plan = MeetingObject::Plan(plan()).to_activity_json();
        assert!(Point::from_activity_json(&plan).is_err());
    }

    #[test]
    fn the_newest_answer_counts_and_stray_choices_do_not() {
        let a1 = MeetingObject::Answer {
            plan: "urn:uuid:1".into(),
            rsvp: Rsvp::No,
            date: AnswerFor::First,
        };
        let a2 = MeetingObject::Answer {
            plan: "urn:uuid:1".into(),
            rsvp: Rsvp::Going,
            date: AnswerFor::First,
        };
        let t = tally_rsvps(
            &plan(),
            plan().start_ms,
            [("bob", &a1), ("ann", &a2), ("bob", &a2)],
        );
        assert_eq!(
            t,
            vec![("ann".into(), Rsvp::Going), ("bob".into(), Rsvp::Going)]
        );

        let poll = Poll {
            id: "p".into(),
            question: "?".into(),
            options: vec!["Fri".into(), "Sat".into()],
            multiple: false,
        };
        let v1 = MeetingObject::Vote {
            poll: "p".into(),
            choices: vec!["Fri".into(), "Sat".into()],
        };
        let v2 = MeetingObject::Vote {
            poll: "p".into(),
            choices: vec!["Sun".into()],
        };
        let t = tally_votes(&poll, [("ann", &v1), ("bob", &v2)]);
        // One choice in a single-choice poll; Sunday is not an option.
        assert_eq!(
            t,
            vec![("Fri".into(), vec!["ann".into()]), ("Sat".into(), vec![])]
        );
    }

    #[test]
    fn a_record_gathers_what_was_said_while_the_call_ran() {
        let start = 1_792_000_800_000;
        let poll = Poll {
            id: "p".into(),
            question: "Which day?".into(),
            options: vec!["Fri".into(), "Sat".into()],
            multiple: false,
        };
        let objects = [
            MeetingObject::Plan(Plan {
                start_ms: start + 5 * 60_000,
                ..plan()
            }),
            MeetingObject::Note {
                kind: NoteKind::Note,
                text: "said the day before".into(),
            },
            MeetingObject::Note {
                kind: NoteKind::Decision,
                text: "Gazebos stay".into(),
            },
            MeetingObject::Poll(poll),
            MeetingObject::Vote {
                poll: "p".into(),
                choices: vec!["Sat".into()],
            },
        ];
        let said = vec![
            ("ann".to_string(), start - 3_600_000, &objects[0]),
            ("ann".to_string(), start - 86_400_000, &objects[1]),
            ("ann".to_string(), start + 60_000, &objects[2]),
            ("bob".to_string(), start + 120_000, &objects[3]),
            ("carol".to_string(), start + 130_000, &objects[4]),
        ];
        let r = build_record(
            &said,
            start,
            start + 1_800_000,
            vec!["bob".into(), "ann".into(), "bob".into()],
            vec![],
            "#q4",
        );
        assert_eq!(r.title, "Q4 planning");
        assert_eq!(r.people, vec!["ann".to_string(), "bob".to_string()]);
        assert_eq!(r.notes.len(), 1, "the day before is not this meeting");
        assert_eq!(
            r.polls[0].results,
            vec![("Fri".into(), 0), ("Sat".into(), 1)]
        );
        assert!(r.has_content());
    }

    #[test]
    fn a_calendar_file_escapes_and_has_a_reminder() {
        let mut p = plan();
        p.title = "Budget, again; really".into();
        let f = ics(&p, "#q4-planning", None);
        assert!(f.contains("SUMMARY:Budget\\, again\\; really\r\n"));
        assert!(f.contains("DTSTART:20261014T180000Z\r\n"));
        assert!(f.contains("TRIGGER:-PT10M"));
    }

    #[test]
    fn an_agenda_line_reads_as_the_web_chat_reads_it() {
        let item = |line: &str| AgendaItem::from_line(line).map(|i| (i.name, i.minutes));
        let named = |name: &str, minutes| Some((name.to_string(), minutes));
        assert_eq!(item("Budget 10"), named("Budget", 10));
        assert_eq!(
            item("- Stage and sound · 15 min"),
            named("Stage and sound", 15)
        );
        assert_eq!(item("Who does what: 5m"), named("Who does what", 5));
        assert_eq!(item("Wrap-up, 20 minutes"), named("Wrap-up", 20));
        assert_eq!(item("Program"), named("Program", 0));
        assert_eq!(
            item("Q4"),
            named("Q4", 0),
            "a number needs a space before it"
        );
        assert_eq!(
            item("Year 2027"),
            named("Year 2027", 0),
            "four digits are a name"
        );
        assert_eq!(item("   "), None);
        assert_eq!(item("•"), None);
    }

    /// Amsterdam's offset in 2026 and early 2027: summer time from
    /// 2026-03-29T01:00Z to 2026-10-25T01:00Z and again from 2027-03-28T01:00Z.
    fn amsterdam(ms: u64) -> i64 {
        if (1_774_746_000_000..1_792_890_000_000).contains(&ms) || ms >= 1_806_195_600_000 {
            7200
        } else {
            3600
        }
    }

    /// Thursday 22 October 2026, 20:00 in Amsterdam (18:00Z, summer time).
    const THU_22_OCT: u64 = 1_792_692_000_000;
    const H: u64 = 3_600_000;
    const WEEK: u64 = 7 * 24 * H;

    fn weekly(until_ms: Option<u64>) -> Plan {
        Plan {
            start_ms: THU_22_OCT,
            end_ms: Some(THU_22_OCT + 90 * 60_000),
            repeat: Some(Repeat {
                every: Every::Week,
                interval: 1,
                until_ms,
            }),
            ..plan()
        }
    }

    #[test]
    fn a_weekly_plan_keeps_its_clock_time_across_the_change_to_winter_time() {
        let p = weekly(None);
        let d = p.dates(0, THU_22_OCT + 3 * WEEK, &amsterdam);
        // 20:00 local each time: 18:00Z in summer time, 19:00Z after 25 October.
        assert_eq!(
            d,
            vec![THU_22_OCT, THU_22_OCT + WEEK + H, THU_22_OCT + 2 * WEEK + H]
        );
        // From a window that starts later, only the dates in it.
        assert_eq!(
            p.dates(THU_22_OCT + 1, THU_22_OCT + WEEK + H + 1, &amsterdam),
            vec![THU_22_OCT + WEEK + H]
        );
        // Every other week.
        let mut two = weekly(None);
        two.repeat.as_mut().unwrap().interval = 2;
        assert_eq!(
            two.dates(0, THU_22_OCT + 3 * WEEK, &amsterdam),
            vec![THU_22_OCT, THU_22_OCT + 2 * WEEK + H]
        );
    }

    #[test]
    fn the_last_date_counts_and_nothing_comes_after_it() {
        // Thursday 17 December, 20:00: nine Thursdays from 22 October.
        let until = 1_797_534_000_000;
        let p = weekly(Some(until));
        let d = p.dates(0, u64::MAX, &amsterdam);
        assert_eq!(d.len(), 9);
        assert_eq!(d.last(), Some(&until));
        assert_eq!(p.date_count(&amsterdam), Some(9));
        assert_eq!(weekly(None).date_count(&amsterdam), None);
        assert_eq!(plan().date_count(&amsterdam), Some(1));
    }

    #[test]
    fn a_monthly_plan_is_the_same_weekday_of_the_month() {
        let p = Plan {
            repeat: Some(Repeat {
                every: Every::Month,
                interval: 1,
                until_ms: None,
            }),
            ..weekly(None)
        };
        // 22 October is the 4th Thursday; then 26 November and 24 December.
        assert_eq!(
            p.dates(0, 1_798_138_800_001, &amsterdam),
            vec![THU_22_OCT, 1_795_719_600_000, 1_798_138_800_000]
        );
        // A 5th Thursday (29 October) passes over months without one: the next
        // is 31 December.
        let fifth = Plan {
            start_ms: 1_793_300_400_000,
            ..p
        };
        assert_eq!(
            fifth.dates(0, 1_798_743_600_001, &amsterdam),
            vec![1_793_300_400_000, 1_798_743_600_000]
        );
    }

    #[test]
    fn the_next_date_passes_over_skipped_and_finished_ones() {
        let mut p = weekly(None);
        let second = THU_22_OCT + WEEK + H;
        p.skip = vec![second];
        assert_eq!(p.next_date(THU_22_OCT - H, &amsterdam), Some(THU_22_OCT));
        // Half an hour in, the first date has not ended yet.
        assert_eq!(
            p.next_date(THU_22_OCT + H / 2, &amsterdam),
            Some(THU_22_OCT)
        );
        // After it, the skipped one is passed over.
        assert_eq!(
            p.next_date(THU_22_OCT + 2 * H, &amsterdam),
            Some(THU_22_OCT + 2 * WEEK + H)
        );
        assert!(p.is_skipped(second));
        // A plan that does not repeat has no next date once it is over.
        assert_eq!(plan().next_date(plan().start_ms + 2 * H, &amsterdam), None);
    }

    #[test]
    fn an_answer_is_for_its_date_and_the_newest_one_counts() {
        let p = weekly(None);
        let (first, second) = (THU_22_OCT, THU_22_OCT + WEEK + H);
        let answer = |rsvp, date| MeetingObject::Answer {
            plan: p.id.clone(),
            rsvp,
            date,
        };
        let every = answer(Rsvp::Going, AnswerFor::Every);
        let maybe_second = answer(Rsvp::Maybe, AnswerFor::Date(second));
        let old_build = answer(Rsvp::No, AnswerFor::First);
        let said = [("ann", &every), ("ann", &maybe_second), ("bob", &old_build)];
        assert_eq!(
            tally_rsvps(&p, first, said),
            vec![("ann".into(), Rsvp::Going), ("bob".into(), Rsvp::No)],
            "an answer naming no date is for the first one"
        );
        assert_eq!(
            tally_rsvps(&p, second, said),
            vec![("ann".into(), Rsvp::Maybe)]
        );
        // Answering every date again afterwards wins over the one date.
        let said = [("ann", &maybe_second), ("ann", &every)];
        assert_eq!(
            tally_rsvps(&p, second, said),
            vec![("ann".into(), Rsvp::Going)]
        );
    }

    #[test]
    fn only_the_author_can_skip_or_end_a_series() {
        let p = weekly(None);
        let second = THU_22_OCT + WEEK + H;
        let skipped = MeetingObject::Update(Plan {
            skip: vec![second],
            ..p.clone()
        });
        let hijack = MeetingObject::Update(Plan {
            title: "Something else".into(),
            ..p.clone()
        });
        let other_id = MeetingObject::Update(Plan {
            id: "urn:uuid:9".into(),
            ..p.clone()
        });
        let now = current_plan(
            &p,
            "ann",
            [("ann", &skipped), ("bob", &hijack), ("ann", &other_id)],
        );
        assert_eq!(now.skip, vec![second]);
        assert_eq!(now.title, p.title);
        assert_eq!(now.id, p.id);
    }

    #[test]
    fn a_change_says_what_it_changed() {
        let p = weekly(None);
        let d = |ms: u64| rfc3339_utc(ms)[..10].to_string();
        let second = THU_22_OCT + WEEK + H;
        let skipped = Plan {
            skip: vec![second],
            ..p.clone()
        };
        assert_eq!(
            PlanChange::between(&p, &skipped).words("Buurtvergadering", &d),
            "skipped Buurtvergadering on 2026-10-29"
        );
        assert_eq!(
            PlanChange::between(&skipped, &p),
            PlanChange::Restored(vec![second])
        );
        let ended = weekly(Some(second));
        assert_eq!(PlanChange::between(&p, &ended), PlanChange::Ends(second));
        assert_eq!(PlanChange::between(&ended, &p), PlanChange::NoEnd);
        let moved = Plan {
            start_ms: THU_22_OCT + H,
            ..p.clone()
        };
        assert_eq!(PlanChange::between(&p, &moved), PlanChange::Other);
    }

    #[test]
    fn repeats_answers_and_updates_read_back_as_written() {
        let p = Plan {
            skip: vec![THU_22_OCT + WEEK + H],
            ..weekly(Some(1_797_534_000_000))
        };
        for o in [
            MeetingObject::Plan(p.clone()),
            MeetingObject::Update(p.clone()),
            MeetingObject::Answer {
                plan: p.id.clone(),
                rsvp: Rsvp::Going,
                date: AnswerFor::Date(THU_22_OCT),
            },
            MeetingObject::Answer {
                plan: p.id.clone(),
                rsvp: Rsvp::No,
                date: AnswerFor::Every,
            },
        ] {
            let json = o.to_activity_json();
            assert_eq!(MeetingObject::from_activity_json(&json), Some(o), "{json}");
        }
        // Skipped dates mean nothing without a rule, and are dropped.
        let one_off = Plan {
            skip: vec![THU_22_OCT],
            ..plan()
        };
        let read =
            MeetingObject::from_activity_json(&MeetingObject::Plan(one_off).to_activity_json());
        assert_eq!(read, Some(MeetingObject::Plan(plan())));
    }

    #[test]
    fn a_repeating_calendar_file_repeats_in_local_time() {
        let p = Plan {
            skip: vec![THU_22_OCT + WEEK + H],
            ..weekly(Some(1_797_534_000_000))
        };
        let f = ics(&p, "#general", Some(("Europe/Amsterdam", &amsterdam)));
        assert!(
            f.contains("DTSTART;TZID=Europe/Amsterdam:20261022T200000\r\n"),
            "{f}"
        );
        assert!(
            f.contains("DTEND;TZID=Europe/Amsterdam:20261022T213000\r\n"),
            "{f}"
        );
        assert!(
            f.contains("RRULE:FREQ=WEEKLY;INTERVAL=1;UNTIL=20261217T190000Z\r\n"),
            "{f}"
        );
        assert!(
            f.contains("EXDATE;TZID=Europe/Amsterdam:20261029T200000\r\n"),
            "{f}"
        );
        let monthly = Plan {
            repeat: Some(Repeat {
                every: Every::Month,
                interval: 1,
                until_ms: None,
            }),
            ..weekly(None)
        };
        let f = ics(&monthly, "#general", Some(("Europe/Amsterdam", &amsterdam)));
        assert!(
            f.contains("RRULE:FREQ=MONTHLY;INTERVAL=1;BYDAY=4TH\r\n"),
            "{f}"
        );
    }
}
