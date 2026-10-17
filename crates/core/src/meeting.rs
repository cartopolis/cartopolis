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

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::point::{parse_rfc3339, rfc3339_utc};

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
}

/// One item of an agenda, and the minutes it was given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgendaItem {
    pub name: String,
    #[serde(default)]
    pub minutes: u32,
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
    },
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
            MeetingObject::Plan(p) => serde_json::json!({
                "@context": CONTEXT,
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
            }),
            MeetingObject::Answer { plan, rsvp } => serde_json::json!({
                "@context": CONTEXT,
                "type": rsvp.kind(),
                "object": plan,
            }),
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
            "Event" if v.get("cartopolis:call").is_some() => MeetingObject::Plan(Plan {
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
            }),
            kind @ ("Accept" | "TentativeAccept" | "Reject") => MeetingObject::Answer {
                plan: s("object")?.to_string(),
                rsvp: match kind {
                    "Accept" => Rsvp::Going,
                    "TentativeAccept" => Rsvp::Maybe,
                    _ => Rsvp::No,
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

/// Counted answers: per person the newest wins, so changing one's mind is
/// sending again. `answers` are (who, object) in the order they were sent.
#[must_use]
pub fn tally_rsvps<'a>(
    plan: &str,
    answers: impl IntoIterator<Item = (&'a str, &'a MeetingObject)>,
) -> Vec<(String, Rsvp)> {
    let mut out: Vec<(String, Rsvp)> = Vec::new();
    for (who, o) in answers {
        if let MeetingObject::Answer { plan: p, rsvp } = o {
            if p == plan {
                out.retain(|(w, _)| w != who);
                out.push((who.to_string(), *rsvp));
            }
        }
    }
    out
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

/// A plan as an iCalendar file, for a person's own calendar. Local times are
/// the calendar's business: the event is written in UTC.
#[must_use]
pub fn ics(plan: &Plan, chat: &str) -> String {
    let stamp = |ms: u64| rfc3339_utc(ms).replace(['-', ':'], "");
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace(';', "\\;")
            .replace(',', "\\,")
            .replace('\n', "\\n")
    };
    let end = plan.end_ms.unwrap_or(plan.start_ms + 60 * 60_000);
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
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Cartopolis//Meetings//EN\r\nBEGIN:VEVENT\r\nUID:{}@cartopolis\r\nDTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:{}\r\nDESCRIPTION:{}\r\nLOCATION:{}\r\nBEGIN:VALARM\r\nTRIGGER:-PT10M\r\nACTION:DISPLAY\r\nDESCRIPTION:{}\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        plan.id.trim_start_matches("urn:uuid:"),
        stamp(plan.start_ms),
        stamp(plan.start_ms),
        stamp(end),
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
        }
    }

    #[test]
    fn every_kind_reads_back_as_written() {
        let all = [
            MeetingObject::Plan(plan()),
            MeetingObject::Answer {
                plan: "urn:uuid:1".into(),
                rsvp: Rsvp::Maybe,
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
        };
        let a2 = MeetingObject::Answer {
            plan: "urn:uuid:1".into(),
            rsvp: Rsvp::Going,
        };
        let t = tally_rsvps("urn:uuid:1", [("bob", &a1), ("ann", &a2), ("bob", &a2)]);
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
        let f = ics(&p, "#q4-planning");
        assert!(f.contains("SUMMARY:Budget\\, again\\; really\r\n"));
        assert!(f.contains("DTSTART:20261014T180000Z\r\n"));
        assert!(f.contains("TRIGGER:-PT10M"));
    }
}
