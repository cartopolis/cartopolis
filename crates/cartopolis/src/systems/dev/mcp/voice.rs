//! **The voice agent**: calls this account takes part in by speech, through
//! OpenAI's Realtime model (`cartopolis_voice`), with this client's MCP tools
//! as its functions (`docs/mockups/voice-agent/`).
//!
//! On with `--voice-agent`, on a headless bot. Three ways into a call:
//!
//! - **Answered.** A direct call from one of the people the agent works for
//!   (the `--mcp-allow` list) is picked up. Anybody else's rings out, as their
//!   chat goes unread.
//! - **Placed.** `voice_call` (the text agent's tool, or the voice agent's own
//!   during a call) queues a call to anybody, with a purpose and who asked.
//!   When it ends, whoever asked gets one line in their private chat. Somebody
//!   who is not a contact yet is sent a request first.
//! - **A meeting.** A group call with one of those people in it is joined; the
//!   agent listens, and speaks when it is addressed.
//!
//! The agent is **a device of the call**: it hears what this client's session
//! decodes and speaks through its encoder, so the call stays end-to-end
//! encrypted up to here. Past here, the audio goes to OpenAI, which is what
//! the account's agent badge says to everybody it talks to.
//!
//! What it may do is decided per tool call in `cartopolis_voice`:
//! [`READ_ONLY`] tools answer anybody in the call, [`FULL`] ones only while
//! the person being answered is one of the people it works for.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use cartopolis_mcp::args::{ContactArgs, SayArgs};
use cartopolis_mcp::voice_tools::McpTools;
use cartopolis_voice::{Config as AgentConfig, Event as AgentEvent, VoiceAgent};
use futures_channel::oneshot;

use super::agent::{AgentRequest, AgentState, Audience};
use super::{Job, JobSender, Reply, Request};
use crate::resources::{AuthIdentity, OnlineSession};
use crate::systems::calls::{CallRequest, Calls};
use crate::systems::chat::ChatState;
use crate::systems::contacts::Contacts;
use crate::systems::private_chat::PrivateChats;

/// Tools anybody in the call may make the agent use: looking, not acting.
pub const READ_ONLY: &[&str] = &[
    "me",
    "look_around",
    "find_place",
    "my_places",
    "recall",
    "call_status",
    "look",
];

/// Tools only the people it works for may make it use.
pub const FULL: &[&str] = &[
    "walk_to",
    "teleport",
    "follow",
    "stop",
    "face",
    "sit",
    "stand",
    "interact",
    "emote",
    "say",
    "send_picture",
    "save_place",
    "remember",
    "build",
    "set_script",
    "edit_object",
    "delete_objects",
    "my_builds",
    "script_output",
    "scripting_guide",
    "voice_call",
];

/// The brief when `--voice-brief` names none.
const DEFAULT_BRIEF: &str = include_str!("voice_brief.md");

/// How long a placed call waits for somebody to accept a contact request.
const CONTACT_PATIENCE: Duration = Duration::from_secs(24 * 3600);
/// How often a chat with a new contact is asked for again while it opens.
const REOPEN_CHAT: Duration = Duration::from_secs(30);
/// A join just asked for has this long to show up as the active call.
const JOIN_GRACE: Duration = Duration::from_secs(5);
/// How long an ended call's agent may take to write its outcome line.
const CLOSE_WAIT: Duration = Duration::from_secs(30);
/// A meeting is often quiet for a long time; this is not a phone line.
const MEETING_IDLE: Duration = Duration::from_secs(3 * 3600);

/// How the voice agent was configured at launch.
#[derive(Resource, Clone, Debug)]
pub struct VoiceSettings {
    /// The Realtime endpoint, with the model in its query.
    pub url: String,
    /// A file holding the key, read afresh for every call, so the host can
    /// keep a short-lived secret in it; else `OPENAI_API_KEY`.
    pub key_file: Option<std::path::PathBuf>,
    pub voice: String,
    pub brief: String,
    pub idle_hangup: Duration,
}

impl VoiceSettings {
    /// From the flags.
    pub fn resolve(
        url: Option<&str>,
        model: &str,
        voice: &str,
        brief: Option<&str>,
        key_file: Option<&str>,
        idle_hangup_s: u64,
    ) -> Result<Self, String> {
        let brief = match brief {
            Some(path) => std::fs::read_to_string(path)
                .map_err(|e| format!("--voice-brief {path}: {e}"))?,
            None => DEFAULT_BRIEF.to_string(),
        };
        let base = url.unwrap_or("wss://api.openai.com/v1/realtime");
        let sep = if base.contains('?') { '&' } else { '?' };
        Ok(Self {
            url: format!("{base}{sep}model={model}"),
            key_file: key_file.map(Into::into),
            voice: voice.to_string(),
            brief,
            idle_hangup: Duration::from_secs(idle_hangup_s.max(10)),
        })
    }

    /// The key for a call starting now, or why there is none.
    fn key(&self) -> Option<String> {
        let key = match &self.key_file {
            Some(path) => std::fs::read_to_string(path).ok(),
            None => std::env::var("OPENAI_API_KEY").ok(),
        };
        key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
    }
}

/// A call somebody asked the agent to place.
#[derive(Debug, Clone)]
pub struct Outbound {
    pub to: String,
    pub purpose: String,
    pub asked_by: String,
    asked_at: Instant,
    chat_asked_at: Option<Instant>,
    contact_asked: bool,
}

/// Calls waiting to be placed: `voice_call` adds to it, [`drive_voice`]
/// places them one at a time.
#[derive(Resource, Default)]
pub struct VoiceCalls {
    queue: VecDeque<Outbound>,
}

impl VoiceCalls {
    /// Queue a call; answers how many are ahead of it.
    pub fn ask(&mut self, to: &str, purpose: &str, asked_by: &str) -> usize {
        self.queue.push_back(Outbound {
            to: to.to_string(),
            purpose: purpose.to_string(),
            asked_by: asked_by.to_string(),
            asked_at: Instant::now(),
            chat_asked_at: None,
            contact_asked: false,
        });
        self.queue.len() - 1
    }
}

enum Kind {
    Answered { caller: String },
    Placed(Outbound),
    Meeting { label: String },
}

struct Live {
    group: Vec<u8>,
    kind: Kind,
    agent: Option<VoiceAgent>,
    since: Instant,
    names: HashMap<u64, String>,
}

struct Closing {
    agent: VoiceAgent,
    /// Who gets the outcome line, and whom it is about.
    report: Option<(String, String)>,
    since: Instant,
}

/// The agent's calls, as they stand.
#[derive(Resource, Default)]
pub struct VoiceState {
    live: Option<Live>,
    closing: Vec<Closing>,
    /// Rings already decided on, so each is answered or declined once.
    handled: HashSet<Vec<u8>>,
    /// Lines and requests sent through the tools, kept until answered: a
    /// waiter whose caller went away is dropped unsent.
    replies: Vec<oneshot::Receiver<Reply>>,
    walking: bool,
}

impl VoiceState {
    /// Ask the account to do something, as a tool call would.
    fn ask(&mut self, jobs: &JobSender, request: AgentRequest) {
        let (tx, rx) = oneshot::channel();
        if jobs
            .0
            .unbounded_send(Job {
                request: Request::Agent(request),
                reply: tx,
            })
            .is_ok()
        {
            self.replies.push(rx);
        }
    }

    /// One line in somebody's private chat with the agent.
    fn tell(&mut self, jobs: &JobSender, to: &str, text: String) {
        tracing::info!(%to, "voice: a line for whoever asked");
        self.ask(
            jobs,
            AgentRequest::Say(SayArgs {
                text,
                to: Some(to.to_string()),
                scope: None,
            }),
        );
    }
}

/// Everything [`drive_voice`] reads and writes, as one parameter.
#[derive(bevy::ecs::system::SystemParam)]
pub struct VoiceWorld<'w> {
    settings: Res<'w, VoiceSettings>,
    queue: ResMut<'w, VoiceCalls>,
    calls: ResMut<'w, Calls>,
    private: Res<'w, PrivateChats>,
    identity: Res<'w, AuthIdentity>,
    audience: Res<'w, Audience>,
    jobs: Res<'w, JobSender>,
    contacts: Res<'w, Contacts>,
    chat: ResMut<'w, ChatState>,
    agent: Res<'w, AgentState>,
    online: Option<Res<'w, OnlineSession>>,
}

/// Answer, place and follow the agent's calls. Once a frame, after the call
/// system, whose state it reads.
pub fn drive_voice(mut state: ResMut<VoiceState>, mut w: VoiceWorld) {
    // The client has no microphone: in a call, the agent is its voice.
    w.calls.agent_voice = true;
    let state = &mut *state;
    state
        .replies
        .retain_mut(|rx| matches!(rx.try_recv(), Ok(None)));
    follow_closing(state, &w);
    if w.online.is_none() {
        state.live = None;
        return;
    }
    let me = w.identity.username.clone().unwrap_or_default();
    let active = w.calls.active_group().map(<[u8]>::to_vec);

    // The call in progress.
    if let Some(live) = state.live.as_mut() {
        if active.as_deref() == Some(live.group.as_slice()) {
            follow_live(live, &mut w, &mut state.walking);
        } else if w.calls.request.is_none() && live.since.elapsed() > JOIN_GRACE {
            if let Some(live) = state.live.take() {
                end_live(state, &w, live);
            }
        }
    }
    state.walking = w.agent.walk_status().0;

    // A call this client is in that the agent is not following yet: one the
    // text agent answered or placed with the plain tools.
    if state.live.is_none() {
        if let Some(group) = active.clone() {
            let kind = match w.private.describe(&group) {
                Some((false, _)) => Kind::Answered {
                    caller: w
                        .calls
                        .others()
                        .first()
                        .map(|(_, n)| n.clone())
                        .unwrap_or_default(),
                },
                Some((true, label)) => Kind::Meeting { label },
                None => Kind::Meeting {
                    label: String::new(),
                },
            };
            state.live = Some(Live {
                group,
                kind,
                agent: None,
                since: Instant::now(),
                names: HashMap::new(),
            });
        }
    }

    answer_rings(state, &mut w, &me);
    if state.live.is_none() && active.is_none() && w.calls.request.is_none() {
        place_next(state, &mut w, &me);
    }
}

/// Ringing calls: pick up the ones from people the agent works for, decline
/// those while it is busy, let the rest ring out.
fn answer_rings(state: &mut VoiceState, w: &mut VoiceWorld, me: &str) {
    let ringing = w.calls.ringing(me);
    state
        .handled
        .retain(|g| ringing.iter().any(|r| &r.group == g));
    let busy_with = state.live.as_ref().map(|l| l.group.clone());
    for r in ringing {
        if state.handled.contains(&r.group) || busy_with.as_ref() == Some(&r.group) {
            continue;
        }
        let Some(caller) = r.who.iter().find(|n| w.audience.hears(n)).cloned() else {
            // Nobody it works for is in it: it rings out, unanswered.
            continue;
        };
        state.handled.insert(r.group.clone());
        let busy = busy_with.is_some() || w.calls.active_group().is_some();
        let no_voice = w.settings.key().is_none();
        if busy || no_voice {
            w.calls.request = Some(CallRequest::Dismiss(r.group.clone()));
            let line = if busy {
                "I'm on another call right now; I'll be free soon."
            } else {
                "Voice isn't available right now; write to me instead."
            };
            state.tell(&w.jobs, &caller, line.into());
            continue;
        }
        let kind = match w.private.describe(&r.group) {
            Some((false, _)) => Kind::Answered { caller },
            Some((true, label)) => Kind::Meeting { label },
            None => continue,
        };
        tracing::info!(from = %r.who.join(", "), "voice: answering");
        w.calls.notice = None;
        w.calls.camera_on = false;
        w.calls.request = Some(CallRequest::Join(r.group.clone()));
        state.live = Some(Live {
            group: r.group,
            kind,
            agent: None,
            since: Instant::now(),
            names: HashMap::new(),
        });
        return;
    }
}

/// Place the first queued call that can be placed, and move the others along:
/// open the chat with a contact, or send a request to somebody who is not one.
fn place_next(state: &mut VoiceState, w: &mut VoiceWorld, me: &str) {
    let now = Instant::now();
    let mut i = 0;
    while i < w.queue.queue.len() {
        let o = &mut w.queue.queue[i];
        if now.duration_since(o.asked_at) > CONTACT_PATIENCE {
            let o = w.queue.queue.remove(i).expect("in range");
            let line = format!(
                "I couldn't call {}: they haven't accepted my contact request in a day.",
                o.to
            );
            state.tell(&w.jobs, &o.asked_by, line);
            continue;
        }
        if let Some(group) = w.private.direct_with(&o.to) {
            let o = w.queue.queue.remove(i).expect("in range");
            if w.settings.key().is_none() {
                let line = format!("Voice isn't available right now, so I couldn't call {}.", o.to);
                state.tell(&w.jobs, &o.asked_by, line);
                return;
            }
            tracing::info!(to = %o.to, "voice: placing a call");
            w.calls.notice = None;
            w.calls.camera_on = false;
            w.calls.request = Some(CallRequest::Join(group.clone()));
            state.live = Some(Live {
                group,
                kind: Kind::Placed(o),
                agent: None,
                since: now,
                names: HashMap::new(),
            });
            return;
        }
        if w.contacts.is_contact(&o.to) || w.contacts.sent_to(&o.to) || o.contact_asked {
            // A contact (or one asked) whose chat is not open on this device
            // yet: ask for it, now and then.
            if o
                .chat_asked_at
                .is_none_or(|at| now.duration_since(at) > REOPEN_CHAT)
            {
                o.chat_asked_at = Some(now);
                w.chat.private_start.push(o.to.clone());
            }
        } else if w.contacts.loaded {
            o.contact_asked = true;
            let (to, asked_by) = (o.to.clone(), o.asked_by.clone());
            state.ask(
                &w.jobs,
                AgentRequest::RequestContact(ContactArgs {
                    username: to.clone(),
                    message: Some(format!(
                        "Hi, I'm {me}, {asked_by}'s assistant. {asked_by} asked me to call you; \
                         accept this and I'll ring."
                    )),
                }),
            );
            let line = format!(
                "{to} isn't my contact yet, so I've sent them a request. I'll call as soon as \
                 they accept."
            );
            state.tell(&w.jobs, &asked_by, line);
        }
        i += 1;
    }
}

/// A call the agent is in: start talking once somebody answers, keep the names
/// up to date, and pass on what the agent wants.
fn follow_live(live: &mut Live, w: &mut VoiceWorld, walking: &mut bool) {
    let names: HashMap<u64, String> = w.calls.others().into_iter().collect();
    if live.agent.is_none() && w.calls.answered() {
        let Some(key) = w.settings.key() else {
            w.calls.request = Some(CallRequest::Leave);
            return;
        };
        let Some((sink, hear)) = w.calls.agent_audio() else {
            return;
        };
        let cfg = agent_config(live, w, key, &names);
        tracing::info!(trusted = ?cfg.trusted, "voice: talking");
        let tools = Arc::new(McpTools::new(w.jobs.0.clone()));
        let agent = VoiceAgent::start(cfg, tools, hear, Box::new(move |f| sink.send(f)));
        live.agent = Some(agent);
    }
    let Some(agent) = live.agent.as_ref() else {
        return;
    };
    if names != live.names {
        agent.names(names.clone());
        live.names = names;
    }
    for ev in agent.events() {
        match ev {
            AgentEvent::HangUp => w.calls.request = Some(CallRequest::Leave),
            AgentEvent::Failed(why) => {
                tracing::warn!(%why, "voice: the agent stopped");
                w.calls.request = Some(CallRequest::Leave);
            }
            AgentEvent::Outcome(_) | AgentEvent::Closed => {}
        }
    }
    // A walk it started has ended: tell it, so it can say so.
    let (now_walking, ended) = w.agent.walk_status();
    if *walking && !now_walking {
        let how = ended.unwrap_or("stopped");
        agent.tell(&format!("Your walk has ended: {how}."), true);
    }
}

/// The call is over: let the agent write its outcome, or say it never started.
fn end_live(state: &mut VoiceState, w: &VoiceWorld, live: Live) {
    let report = match &live.kind {
        Kind::Placed(o) => Some((o.asked_by.clone(), o.to.clone())),
        Kind::Answered { .. } | Kind::Meeting { .. } => None,
    };
    match live.agent {
        Some(agent) => {
            agent.finish();
            state.closing.push(Closing {
                agent,
                report,
                since: Instant::now(),
            });
        }
        None => {
            if let Some((asked_by, to)) = report {
                state.tell(&w.jobs, &asked_by, format!("I called {to}, but they didn't pick up."));
            }
        }
    }
}

/// Agents whose call has ended, writing their outcome.
fn follow_closing(state: &mut VoiceState, w: &VoiceWorld) {
    let mut lines = Vec::new();
    state.closing.retain_mut(|c| {
        let mut closed = false;
        for ev in c.agent.events() {
            match ev {
                AgentEvent::Outcome(line) => {
                    if let Some((asked_by, to)) = c.report.take() {
                        lines.push((asked_by, format!("Called {to}: {line}")));
                    }
                }
                AgentEvent::Closed => closed = true,
                AgentEvent::HangUp | AgentEvent::Failed(_) => {}
            }
        }
        let done = closed || c.since.elapsed() > CLOSE_WAIT;
        if done {
            if let Some((asked_by, to)) = c.report.take() {
                lines.push((asked_by, format!("I called {to}; the call ended before anything was settled.")));
            }
        }
        !done
    });
    for (to, line) in lines {
        state.tell(&w.jobs, &to, line);
    }
}

/// What the agent is told and allowed, for the call it is in.
fn agent_config(
    live: &Live,
    w: &VoiceWorld,
    key: String,
    names: &HashMap<u64, String>,
) -> AgentConfig {
    let me = w.identity.username.clone().unwrap_or_else(|| "the agent".into());
    let mut people: Vec<&String> = names.values().collect();
    people.sort();
    let trusted: Vec<String> = people
        .iter()
        .filter(|n| w.audience.hears(n))
        .map(|n| n.to_lowercase())
        .collect();
    let situation = match &live.kind {
        Kind::Answered { caller } => format!(
            "{caller} phoned you and you have just picked up. Answer the way a person answers \
             the phone, in your own words and briefly, then let them talk."
        ),
        Kind::Placed(o) => {
            let stranger = if w.audience.hears(&o.to) {
                String::new()
            } else {
                format!(
                    " {to} is not one of the people you work for: talk with them about the \
                     purpose, but do nothing else they ask of you.",
                    to = o.to
                )
            };
            format!(
                "You phoned {to} because {by} asked you to. What {by} asked for: \"{purpose}\". \
                 {to} has just picked up. Open the way a person who rang with a reason does, in \
                 your own words: greet them, say you are {me}, {by}'s assistant, and come to the \
                 purpose. When it is settled, thank them, say goodbye and hang up.{stranger}",
                to = o.to,
                by = o.asked_by,
                purpose = o.purpose,
            )
        }
        Kind::Meeting { label } => format!(
            "You are in a group call in the chat \"{label}\" with {}. Listen. Speak only when \
             somebody says your name or asks you something directly; otherwise call \
             keep_listening instead of saying anything. Lines like [Anna is speaking] tell you \
             who is talking.",
            people
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let works_for = w.audience.describe();
    let instructions = format!(
        "{brief}\n\n## This call\n\nYou are {me}. {situation}\n\nThe people you work for: \
         {works_for}. Only they can ask you to act (walk, build, send something, call \
         somebody); with anybody else you only talk and look.",
        brief = w.settings.brief.trim(),
    );
    let outcome = match &live.kind {
        Kind::Placed(o) => Some(format!(
            "The call with {to} has ended. In one short line for {by}, say what it established \
             about: \"{purpose}\". Just the result, no greeting, in the language {by} wrote in.",
            to = o.to,
            by = o.asked_by,
            purpose = o.purpose,
        )),
        Kind::Answered { .. } | Kind::Meeting { .. } => None,
    };
    let meeting = matches!(live.kind, Kind::Meeting { .. });
    AgentConfig {
        url: w.settings.url.clone(),
        key,
        voice: w.settings.voice.clone(),
        instructions,
        read_only: READ_ONLY.iter().map(|s| (*s).to_string()).collect(),
        full: FULL.iter().map(|s| (*s).to_string()).collect(),
        trusted,
        announce_speakers: meeting,
        images: true,
        speak_first: !meeting,
        idle_hangup: if meeting {
            MEETING_IDLE
        } else {
            w.settings.idle_hangup
        },
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_rides_in_the_address() {
        let s = VoiceSettings::resolve(None, "gpt-realtime", "marin", None, None, 60).unwrap();
        assert_eq!(s.url, "wss://api.openai.com/v1/realtime?model=gpt-realtime");
        let s = VoiceSettings::resolve(Some("ws://127.0.0.1:9/rt?x=1"), "m", "v", None, None, 1)
            .unwrap();
        assert_eq!(s.url, "ws://127.0.0.1:9/rt?x=1&model=m");
        assert_eq!(s.idle_hangup, Duration::from_secs(10));
        assert!(s.brief.contains("phone"));
    }

    #[test]
    fn no_tool_is_both_read_only_and_full() {
        for t in READ_ONLY {
            assert!(!FULL.contains(t), "{t}");
        }
    }

    #[test]
    fn calls_queue_in_order() {
        let mut q = VoiceCalls::default();
        assert_eq!(q.ask("anna", "Saturday?", "jeroen"), 0);
        assert_eq!(q.ask("bob", "Sunday?", "jeroen"), 1);
        assert_eq!(q.queue.front().map(|o| o.to.as_str()), Some("anna"));
    }
}
