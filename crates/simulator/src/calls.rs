//! Calls in private chats (protocol 26, `docs/notes/calls.md`): the task that
//! owns the forwarder's UDP socket.
//!
//! One task, one socket, one [`Sfu`] — the forwarder is sans-IO, so this is
//! where its clock and its datagrams come from. Connections talk to it over a
//! channel ([`CallsHandle`]); what it has to tell a device (an offer for its
//! receiving connection, who is in a call) goes back through the chat hub,
//! the same way a private chat's log entries reach a device, so the connection
//! loop grows no new branch for it.
//!
//! The forwarder cannot open anything it forwards, and this crate does not
//! link the code that could (`cartopolis_calls` without its `client` half).
//! Who may join is decided by the connection before it asks: a member of the
//! chat, with a registered device.
//!
//! Since protocol 41 a call is also a meeting ([`Meeting`]): it has a host,
//! raised hands, a waiting room, breakout rooms and settings, and what a host
//! decides is carried out here — held sound and a forbidden screen are dropped
//! at the forwarder ([`Sfu::hold`]), breakout rooms are kept apart there
//! ([`Sfu::set_breakout`]), a removed person is taken out and kept out —
//! rather than asked of the other apps, which could be rewritten to ignore it.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use cartopolis_calls::sfu::{Kind, ParticipantId, Sfu, Signal};
use cartopolis_core::{
    encode_app_frame, CallAction, CallEndReason, CallMember, CallRole, CallRooms, CallSettings,
    CallTrack, CallTrackKind, CallWaiter, NetMessage,
};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

use crate::chat::ChatHub;

/// Longest SDP accepted from a client. An audio-and-video offer is ~3 kB.
pub const MAX_SDP_BYTES: usize = 16 * 1024;

/// Longest sealed call event relayed: a reaction, or a line of captions.
pub const MAX_REACTION_BYTES: usize = 1024;
/// The longest sealed key a host may hand a guest: a base key, a few dozen
/// member indices and the seal, with room.
pub const MAX_KEY_DROP_BYTES: usize = 4096;

/// Most breakout rooms, and the longest room name.
const MAX_ROOMS: usize = 20;
const MAX_ROOM_NAME: usize = 40;

/// A connection's way to the calls task. Cheap to clone.
#[derive(Clone)]
pub struct CallsHandle {
    tx: mpsc::UnboundedSender<Command>,
}

/// Who is asking to join, as the connection established it.
pub struct Joiner {
    pub group: Vec<u8>,
    pub username: String,
    /// `accounts.id`, so a push for the call does not wake the caller's own phone.
    pub account: String,
    pub device: uuid::Uuid,
    /// Usernames this account blocks, lowercased: their media is not forwarded
    /// to it.
    pub blocked: HashSet<String>,
    /// The chat's member accounts, for telling everyone who is in the call.
    pub members: Vec<String>,
    pub offer: String,
    /// The media id in `offer` that carries a shared screen.
    pub screen: Option<String>,
    /// The join's request id, for the `CallJoined` an admission sends later.
    pub request_id: u32,
    /// A guest let in by an invite link: always waits to be let in.
    pub guest: bool,
}

/// What became of a join.
pub enum Joined {
    /// In, with the forwarder's answer.
    In(Seat, String),
    /// Waiting to be let in.
    Waiting(Seat),
}

enum Command {
    Join {
        joiner: Joiner,
        reply: oneshot::Sender<Result<JoinReply, String>>,
    },
    Answer {
        participant: u64,
        sdp: String,
    },
    Leave {
        participant: u64,
    },
    /// A device just came online: tell it about every call already running
    /// in a chat its account is in, or a call it was woken for (an Android
    /// push opening the app) never rings, because nothing changed since.
    Catchup {
        account: String,
        device: uuid::Uuid,
        blocked: HashSet<String>,
    },
    Control {
        participant: u64,
        action: CallAction,
        reply: oneshot::Sender<Result<(), String>>,
    },
    React {
        participant: u64,
        epoch: u64,
        sealed: Vec<u8>,
    },
    Watch {
        participant: u64,
        cameras: Option<Vec<u64>>,
    },
    /// A host's sealed key for a guest (`NetMessage::CallKeyDrop`).
    KeyDrop {
        from: u64,
        to: u64,
        sealed: Vec<u8>,
    },
}

enum JoinReply {
    In(u64, String),
    Waiting(u64),
}

/// A connection's place in a call. Dropping it leaves the call, which is what
/// makes a closed connection leave too: the connection holds this in the same
/// scope as its avatar.
pub struct Seat {
    handle: CallsHandle,
    pub group: Vec<u8>,
    pub participant: u64,
}

impl Drop for Seat {
    fn drop(&mut self) {
        let _ = self.handle.tx.send(Command::Leave {
            participant: self.participant,
        });
    }
}

impl CallsHandle {
    /// Join a call, or wait to be let in. Either way the seat is the
    /// connection's until it drops it.
    pub async fn join(&self, joiner: Joiner) -> Result<Joined, String> {
        let group = joiner.group.clone();
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Join { joiner, reply })
            .map_err(|_| "calls are not running on this server".to_string())?;
        let answered = rx
            .await
            .map_err(|_| "calls are not running on this server".to_string())??;
        let seat = |participant| Seat {
            handle: self.clone(),
            group: group.clone(),
            participant,
        };
        Ok(match answered {
            JoinReply::In(p, answer) => Joined::In(seat(p), answer),
            JoinReply::Waiting(p) => Joined::Waiting(seat(p)),
        })
    }

    /// Tell a device that has just registered about the calls already
    /// running in its chats (see [`Command::Catchup`]).
    pub fn catch_up(&self, account: String, device: uuid::Uuid, blocked: HashSet<String>) {
        let _ = self.tx.send(Command::Catchup {
            account,
            device,
            blocked,
        });
    }

    /// The answer to the last offer this seat was sent.
    pub fn answer(&self, seat: &Seat, sdp: String) {
        let _ = self.tx.send(Command::Answer {
            participant: seat.participant,
            sdp,
        });
    }

    /// A hand, a shared screen, or a host's action, from the seat numbered
    /// `participant`. `Err` is the refusal, for a person.
    pub async fn control(&self, participant: u64, action: CallAction) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Control {
                participant,
                action,
                reply,
            })
            .map_err(|_| "calls are not running on this server".to_string())?;
        rx.await
            .map_err(|_| "calls are not running on this server".to_string())?
    }

    /// A sealed call event (a reaction, a line of captions) from this seat,
    /// for everyone else in its call and room.
    pub fn react(&self, seat: &Seat, epoch: u64, sealed: Vec<u8>) {
        let _ = self.tx.send(Command::React {
            participant: seat.participant,
            epoch,
            sealed,
        });
    }

    /// The call's key, sealed for the guest seated as `to`: relayed only from
    /// a host, only to a guest in the same call.
    pub fn key_drop(&self, seat: &Seat, to: u64, sealed: Vec<u8>) {
        let _ = self.tx.send(Command::KeyDrop {
            from: seat.participant,
            to,
            sealed,
        });
    }

    /// Whose cameras this seat draws (`None`: everybody's).
    pub fn watch(&self, seat: &Seat, cameras: Option<Vec<u64>>) {
        let _ = self.tx.send(Command::Watch {
            participant: seat.participant,
            cameras,
        });
    }
}

struct Info {
    group: Vec<u8>,
    username: String,
    account: String,
    device: uuid::Uuid,
    blocked: HashSet<String>,
    role: CallRole,
    /// A raised hand's place in the queue.
    hand: Option<u32>,
    /// A host muted it.
    held: bool,
    /// A host let it talk while members may not unmute.
    allowed: bool,
    sharing: bool,
    /// Its microphone is off, by its own say.
    muted: bool,
    /// Recording the call on itself, by its own say.
    recording: bool,
    guest: bool,
    /// Its breakout room; 0 is the main call.
    room: u32,
}

impl Info {
    fn new(joiner: &Joiner, role: CallRole) -> Self {
        Self {
            group: joiner.group.clone(),
            username: joiner.username.clone(),
            account: joiner.account.clone(),
            device: joiner.device,
            blocked: joiner.blocked.clone(),
            role,
            hand: None,
            held: false,
            allowed: false,
            sharing: false,
            muted: false,
            recording: false,
            guest: joiner.guest,
            room: 0,
        }
    }

    /// Whether its sound is dropped at the forwarder.
    fn sound_held(&self, s: &CallSettings) -> bool {
        self.held || (!s.may_unmute && !self.role.hosts() && !self.allowed)
    }

    /// Whether its screen is dropped at the forwarder: not sharing (so a
    /// picture sent before it said so, or after a host stopped it, goes
    /// nowhere), or sharing is the hosts' alone.
    fn screen_held(&self, s: &CallSettings) -> bool {
        !self.sharing || (!s.may_share && !self.role.hosts())
    }
}

/// A device waiting to be let in, with everything its join will need.
struct Waiter {
    participant: ParticipantId,
    joiner: Joiner,
    since_ms: u64,
}

/// Open breakout rooms.
struct Rooms {
    names: Vec<String>,
    until: Option<Instant>,
    until_ms: Option<u64>,
    may_move: bool,
}

/// A call as a meeting: what outlives any one person in it.
#[derive(Default)]
struct Meeting {
    settings: CallSettings,
    next_hand: u32,
    /// Accounts a host took out: they may not come back while the call lasts.
    removed: HashSet<String>,
    /// Accounts that were in the call, so a locked call still lets them back
    /// in after a dropped connection.
    been_in: HashSet<String>,
    waiting: Vec<Waiter>,
    spotlight: Option<ParticipantId>,
    rooms: Option<Rooms>,
}

/// Why a join is refused before it reaches the forwarder.
fn refuse_join(meeting: Option<&Meeting>, account: &str) -> Option<&'static str> {
    let m = meeting?;
    if m.removed.contains(account) {
        return Some("a host removed you from this call");
    }
    if m.settings.locked && !m.been_in.contains(account) {
        return Some("the call is locked");
    }
    None
}

/// Whether a join waits for a host: a guest always (once somebody is there to
/// let them in), a member when the hosts say so — but never somebody who was
/// already in, coming back after a dropped connection, and never the first.
fn must_wait(meeting: Option<&Meeting>, joiner: &Joiner, anyone_in: bool) -> bool {
    if !anyone_in {
        return false;
    }
    let Some(m) = meeting else {
        return joiner.guest;
    };
    if m.been_in.contains(&joiner.account) {
        return false;
    }
    joiner.guest || m.settings.members_wait
}

/// What a control did beyond the state it changed, for the task to carry out
/// at the forwarder and on the wire.
#[derive(Debug, Default, PartialEq, Eq)]
struct Outcome {
    /// Who must leave, and why: a removal, or everybody at an end-for-all.
    leaving: Vec<(ParticipantId, CallEndReason)>,
    /// Waiting devices to let in.
    admit: Vec<ParticipantId>,
    /// Waiting devices to turn away.
    deny: Vec<ParticipantId>,
    /// Somebody's breakout room changed.
    rooms_changed: bool,
}

impl Outcome {
    fn none() -> Result<Self, String> {
        Ok(Self::default())
    }
}

/// Carry out `actor`'s `action` on the call's state.
fn control(
    info: &mut HashMap<ParticipantId, Info>,
    meeting: &mut Meeting,
    actor: ParticipantId,
    action: CallAction,
    now: Instant,
    now_ms: u64,
) -> Result<Outcome, String> {
    let (group, role, by, my_room) = match info.get(&actor) {
        Some(i) => (i.group.clone(), i.role, i.username.clone(), i.room),
        None => return Err("you are not in a call".into()),
    };
    let in_call = |p: u64| -> Result<ParticipantId, String> {
        let id = ParticipantId(p);
        match info.get(&id) {
            Some(i) if i.group == group => Ok(id),
            _ => Err("they are no longer in the call".into()),
        }
    };
    // A device's own say first: anybody may.
    match action {
        CallAction::Hand(up) => {
            let me = info.get_mut(&actor).expect("checked above");
            if !up {
                me.hand = None;
            } else if me.hand.is_none() {
                meeting.next_hand += 1;
                me.hand = Some(meeting.next_hand);
            }
            return Outcome::none();
        }
        CallAction::Muted(on) => {
            info.get_mut(&actor).expect("checked above").muted = on;
            return Outcome::none();
        }
        CallAction::Sharing(on) => {
            if on && !meeting.settings.may_share && !role.hosts() {
                return Err("only the hosts may share a screen in this call".into());
            }
            info.get_mut(&actor).expect("checked above").sharing = on;
            return Outcome::none();
        }
        CallAction::Recording(on) => {
            if on && !meeting.settings.may_record {
                return Err("recording is off in this call".into());
            }
            info.get_mut(&actor).expect("checked above").recording = on;
            return Outcome::none();
        }
        CallAction::MoveTo { participant, room } if participant == actor.0 && !role.hosts() => {
            let rooms = meeting.rooms.as_ref().ok_or("no rooms are open")?;
            if !rooms.may_move {
                return Err("the hosts move people between rooms".into());
            }
            if room as usize > rooms.names.len() {
                return Err("no such room".into());
            }
            info.get_mut(&actor).expect("checked above").room = room;
            return Ok(Outcome {
                rooms_changed: true,
                ..Outcome::default()
            });
        }
        _ => {}
    }
    if !role.hosts() {
        return Err("only a host can do that".into());
    }
    let mut out = Outcome::default();
    match action {
        CallAction::Hand(_)
        | CallAction::Sharing(_)
        | CallAction::Muted(_)
        | CallAction::Recording(_) => unreachable!("handled above"),
        CallAction::Mute { participant } => {
            let t = info.get_mut(&in_call(participant)?).expect("in the call");
            t.held = true;
            t.allowed = false;
        }
        CallAction::AllowTalk { participant } => {
            let t = info.get_mut(&in_call(participant)?).expect("in the call");
            t.held = false;
            t.allowed = true;
            t.hand = None;
        }
        CallAction::LowerHand { participant } => {
            info.get_mut(&in_call(participant)?)
                .expect("in the call")
                .hand = None;
        }
        CallAction::StopSharing { participant } => {
            info.get_mut(&in_call(participant)?)
                .expect("in the call")
                .sharing = false;
        }
        CallAction::MuteAll => {
            for i in info
                .values_mut()
                .filter(|i| i.group == group && !i.role.hosts())
            {
                i.held = true;
                i.allowed = false;
            }
        }
        CallAction::Remove { participant } => {
            let id = in_call(participant)?;
            if id == actor {
                return Err("leave the call instead".into());
            }
            let t = &info[&id];
            if t.role == CallRole::Host && role != CallRole::Host {
                return Err("a co-host cannot remove the host".into());
            }
            meeting.removed.insert(t.account.clone());
            out.leaving.push((id, CallEndReason::Removed { by }));
        }
        CallAction::Role {
            participant,
            role: new,
        } => {
            let id = in_call(participant)?;
            match new {
                CallRole::Host => {
                    if role != CallRole::Host {
                        return Err("only the host can hand the call over".into());
                    }
                    info.get_mut(&actor).expect("checked above").role = CallRole::CoHost;
                    info.get_mut(&id).expect("in the call").role = CallRole::Host;
                }
                CallRole::CoHost | CallRole::Member => {
                    let t = info.get_mut(&id).expect("in the call");
                    if t.role == CallRole::Host {
                        return Err("the host stays host until they hand the call over".into());
                    }
                    t.role = new;
                }
            }
        }
        CallAction::Settings(s) => meeting.settings = s,
        CallAction::EndForAll => {
            out.leaving = info
                .iter()
                .filter(|(_, i)| i.group == group)
                .map(|(&id, _)| (id, CallEndReason::Ended { by: by.clone() }))
                .collect();
        }
        CallAction::Admit { participant } | CallAction::Deny { participant } => {
            let id = ParticipantId(participant);
            if !meeting.waiting.iter().any(|w| w.participant == id) {
                return Err("they are no longer waiting".into());
            }
            if matches!(action, CallAction::Admit { .. }) {
                out.admit.push(id);
            } else {
                out.deny.push(id);
            }
        }
        CallAction::AdmitAll => out.admit = meeting.waiting.iter().map(|w| w.participant).collect(),
        CallAction::Spotlight { participant } => {
            meeting.spotlight = match participant {
                Some(p) => Some(in_call(p)?),
                None => None,
            };
        }
        CallAction::OpenRooms {
            names,
            assign,
            minutes,
            may_move,
        } => {
            if names.is_empty()
                || names.len() > MAX_ROOMS
                || names
                    .iter()
                    .any(|n| n.trim().is_empty() || n.chars().count() > MAX_ROOM_NAME)
            {
                return Err(format!(
                    "between 1 and {MAX_ROOMS} rooms, each with a short name"
                ));
            }
            if assign.iter().any(|(_, room)| *room as usize > names.len()) {
                return Err("no such room".into());
            }
            for (id, i) in info.iter_mut().filter(|(_, i)| i.group == group) {
                i.room = assign
                    .iter()
                    .find(|(p, _)| *p == id.0)
                    .map_or(0, |(_, room)| *room);
            }
            let secs = u64::from(minutes) * 60;
            meeting.rooms = Some(Rooms {
                names,
                until: (minutes > 0).then(|| now + Duration::from_secs(secs)),
                until_ms: (minutes > 0).then(|| now_ms + secs * 1000),
                may_move,
            });
            out.rooms_changed = true;
        }
        CallAction::MoveTo { participant, room } => {
            let names = meeting.rooms.as_ref().map_or(0, |r| r.names.len());
            if room as usize > names {
                return Err("no such room".into());
            }
            info.get_mut(&in_call(participant)?)
                .expect("in the call")
                .room = room;
            out.rooms_changed = true;
        }
        CallAction::CloseRooms => {
            close_rooms(info, meeting, &group);
            out.rooms_changed = true;
        }
    }
    let _ = my_room;
    Ok(out)
}

/// Everybody back to the main call.
fn close_rooms(info: &mut HashMap<ParticipantId, Info>, meeting: &mut Meeting, group: &[u8]) {
    meeting.rooms = None;
    for i in info.values_mut().filter(|i| i.group == group) {
        i.room = 0;
    }
}

/// When nobody left in `group` hosts it, whoever has been there longest
/// (first in `order`, the forwarder's join order) does.
fn succeed(info: &mut HashMap<ParticipantId, Info>, order: &[ParticipantId], group: &[u8]) {
    if info.values().any(|i| i.group == group && i.role.hosts()) {
        return;
    }
    if let Some(id) = order.iter().find(|id| info.contains_key(id)) {
        if let Some(i) = info.get_mut(id) {
            i.role = CallRole::Host;
        }
    }
}

/// Make the forwarder do what the meeting says, for everyone in `group`: drop
/// what is held, and keep each breakout room to itself.
fn apply(sfu: &mut Sfu, info: &HashMap<ParticipantId, Info>, meeting: &Meeting, group: &[u8]) {
    let now = Instant::now();
    for (&id, i) in info.iter().filter(|(_, i)| i.group == group) {
        sfu.hold(id, i.sound_held(&meeting.settings));
        sfu.hold_screen(now, id, i.screen_held(&meeting.settings));
        sfu.set_breakout(now, id, i.room);
    }
}

/// Bind the forwarder's socket and start its task. `public` is the address
/// clients reach that socket at; `None` means the bound address, which is
/// right for a loopback or a directly-addressed host and wrong behind NAT.
pub async fn spawn(
    listen: &str,
    public: Option<SocketAddr>,
    chat: Arc<ChatHub>,
    ring: Option<mpsc::UnboundedSender<crate::push::Ring>>,
) -> anyhow::Result<(CallsHandle, SocketAddr, tokio::task::JoinHandle<()>)> {
    let socket = UdpSocket::bind(listen).await?;
    let bound = socket.local_addr()?;
    let public = public.unwrap_or(bound);
    if public.ip().is_unspecified() {
        anyhow::bail!(
            "calls listen on {bound} but no public address was given; clients cannot \
             reach an unspecified address (set calls_public_addr)"
        );
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let state = State {
        sfu: Sfu::new(public),
        chat,
        ring,
        info: HashMap::new(),
        members: HashMap::new(),
        meetings: HashMap::new(),
    };
    let task = tokio::spawn(run(socket, state, rx));
    Ok((CallsHandle { tx }, bound, task))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Everything the calls task holds.
struct State {
    sfu: Sfu,
    chat: Arc<ChatHub>,
    ring: Option<mpsc::UnboundedSender<crate::push::Ring>>,
    info: HashMap<ParticipantId, Info>,
    /// Every member account of a chat whose call has anyone in it, for the
    /// roster. Refreshed at each join, which is when it is read from the table.
    members: HashMap<Vec<u8>, Vec<String>>,
    meetings: HashMap<Vec<u8>, Meeting>,
}

impl State {
    fn send_to(&self, device: uuid::Uuid, msg: &NetMessage) {
        if let Ok(frame) = encode_app_frame(msg) {
            self.chat.deliver_to_device(device, &Bytes::from(frame));
        }
    }

    fn join(&mut self, joiner: Joiner) -> Result<JoinReply, String> {
        let group = joiner.group.clone();
        if let Some(why) = refuse_join(self.meetings.get(&group), &joiner.account) {
            return Err(why.to_string());
        }
        let anyone_in = !self.sfu.members(&group).is_empty();
        if joiner.guest && !anyone_in {
            return Err("the meeting has not started yet".into());
        }
        self.members.insert(group.clone(), joiner.members.clone());
        if must_wait(self.meetings.get(&group), &joiner, anyone_in) {
            let participant = self.sfu.reserve();
            tracing::info!(participant = participant.0, username = %joiner.username, "waiting to join a call");
            self.meetings
                .entry(group.clone())
                .or_default()
                .waiting
                .push(Waiter {
                    participant,
                    joiner,
                    since_ms: now_ms(),
                });
            self.roster(&group, "");
            return Ok(JoinReply::Waiting(participant.0));
        }
        let id = self.sfu.reserve();
        let answer = self.seat(id, joiner)?;
        Ok(JoinReply::In(id.0, answer))
    }

    /// Put `joiner` in the call under `id`: the forwarder, blocks, the
    /// meeting's state, the first one's ring, and the roster.
    fn seat(&mut self, id: ParticipantId, joiner: Joiner) -> Result<String, String> {
        let group = joiner.group.clone();
        let answer = self
            .sfu
            .join_as(
                Instant::now(),
                id,
                group.clone(),
                &joiner.offer,
                joiner.screen.as_deref(),
            )
            .map_err(|e| e.to_string())?;
        for (&other, o) in self.info.iter().filter(|(_, o)| o.group == group) {
            if joiner.blocked.contains(&o.username.to_lowercase()) {
                self.sfu.ignore(id, other);
            }
            if o.blocked.contains(&joiner.username.to_lowercase()) {
                self.sfu.ignore(other, id);
            }
        }
        tracing::info!(participant = id.0, username = %joiner.username, "joined a call");
        // The first one in hosts it.
        let first = self.sfu.members(&group).len() == 1;
        let role = if first {
            CallRole::Host
        } else {
            CallRole::Member
        };
        self.info.insert(id, Info::new(&joiner, role));
        let meeting = self.meetings.entry(group.clone()).or_default();
        meeting.been_in.insert(joiner.account.clone());
        apply(&mut self.sfu, &self.info, meeting, &group);
        // The first one in starts the call: wake the phones of everyone else
        // in the chat whose app is closed (`push`).
        if first {
            if let Some(ring) = &self.ring {
                let _ = ring.send(crate::push::Ring {
                    caller: joiner.account.clone(),
                    caller_name: joiner.username.clone(),
                    members: joiner.members.clone(),
                });
            }
        }
        self.roster(&group, &joiner.username);
        Ok(answer)
    }

    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Join { joiner, reply } => {
                let _ = reply.send(self.join(joiner));
            }
            Command::Answer { participant, sdp } => {
                if let Err(e) = self
                    .sfu
                    .answer(Instant::now(), ParticipantId(participant), &sdp)
                {
                    tracing::debug!(participant, "call answer refused: {e}");
                }
            }
            Command::Leave { participant } => {
                let id = ParticipantId(participant);
                // Somebody who gave up waiting.
                let mut gave_up = None;
                for (group, m) in &mut self.meetings {
                    if let Some(i) = m.waiting.iter().position(|w| w.participant == id) {
                        m.waiting.remove(i);
                        gave_up = Some(group.clone());
                    }
                }
                match gave_up {
                    Some(group) => self.roster(&group, ""),
                    None => self.sfu.leave(id),
                }
            }
            Command::Catchup {
                account,
                device,
                blocked,
            } => {
                let groups: Vec<Vec<u8>> = self
                    .members
                    .iter()
                    .filter(|(_, accounts)| accounts.contains(&account))
                    .map(|(g, _)| g.clone())
                    .collect();
                for group in groups {
                    let msg = self.roster_msg(&group);
                    let NetMessage::CallRoster { members, .. } = &msg else {
                        continue;
                    };
                    // A call made only of people this account blocks does not
                    // ring for it, as `deliver_private` rules for a live roster.
                    if members.is_empty()
                        || members
                            .iter()
                            .all(|m| blocked.contains(&m.username.to_lowercase()))
                    {
                        continue;
                    }
                    self.send_to(device, &msg);
                }
            }
            Command::Control {
                participant,
                action,
                reply,
            } => {
                let _ = reply.send(self.control(ParticipantId(participant), action));
            }
            Command::React {
                participant,
                epoch,
                sealed,
            } => {
                let from = ParticipantId(participant);
                if let Some(sender) = self.info.get(&from) {
                    let msg = NetMessage::CallReacted {
                        group: sender.group.clone(),
                        participant,
                        epoch,
                        sealed,
                    };
                    let name = sender.username.to_lowercase();
                    // Within the sender's breakout room: a room's talk is its own.
                    let to: Vec<uuid::Uuid> = self
                        .info
                        .iter()
                        .filter(|(&id, o)| {
                            id != from
                                && o.group == sender.group
                                && o.room == sender.room
                                && !o.blocked.contains(&name)
                        })
                        .map(|(_, o)| o.device)
                        .collect();
                    for device in to {
                        self.send_to(device, &msg);
                    }
                }
            }
            Command::Watch {
                participant,
                cameras,
            } => {
                self.sfu.watch(
                    Instant::now(),
                    ParticipantId(participant),
                    cameras.map(|c| c.into_iter().map(ParticipantId).collect()),
                );
            }
            Command::KeyDrop { from, to, sealed } => {
                let (Some(host), Some(guest)) = (
                    self.info.get(&ParticipantId(from)),
                    self.info.get(&ParticipantId(to)),
                ) else {
                    return;
                };
                // A host's, for a guest let into the same call: the server
                // cannot open it, so it can only check who and to whom.
                if host.role.hosts() && guest.guest && host.group == guest.group {
                    let msg = NetMessage::CallKeyDropped {
                        group: guest.group.clone(),
                        sealed,
                    };
                    self.send_to(guest.device, &msg);
                }
            }
        }
    }

    fn control(&mut self, actor: ParticipantId, action: CallAction) -> Result<(), String> {
        let group = self
            .info
            .get(&actor)
            .map(|i| i.group.clone())
            .ok_or("you are not in a call")?;
        let meeting = self.meetings.entry(group.clone()).or_default();
        let out = control(
            &mut self.info,
            meeting,
            actor,
            action,
            Instant::now(),
            now_ms(),
        )?;
        for (id, reason) in out.leaving {
            if let Some(i) = self.info.get(&id) {
                self.send_to(
                    i.device,
                    &NetMessage::CallEnded {
                        group: group.clone(),
                        reason,
                    },
                );
            }
            self.sfu.leave(id);
        }
        for id in out.deny {
            let Some(m) = self.meetings.get_mut(&group) else {
                continue;
            };
            let Some(i) = m.waiting.iter().position(|w| w.participant == id) else {
                continue;
            };
            let w = m.waiting.remove(i);
            self.send_to(
                w.joiner.device,
                &crate::net::call_error(w.joiner.request_id, "a host did not let you in"),
            );
        }
        for id in out.admit {
            let Some(m) = self.meetings.get_mut(&group) else {
                continue;
            };
            let Some(i) = m.waiting.iter().position(|w| w.participant == id) else {
                continue;
            };
            let w = m.waiting.remove(i);
            let (device, request_id) = (w.joiner.device, w.joiner.request_id);
            match self.seat(id, w.joiner) {
                Ok(answer) => self.send_to(
                    device,
                    &NetMessage::CallJoined {
                        request_id,
                        group: group.clone(),
                        participant: id.0,
                        answer,
                    },
                ),
                Err(why) => self.send_to(device, &crate::net::call_error(request_id, why)),
            }
        }
        if let Some(meeting) = self.meetings.get(&group) {
            apply(&mut self.sfu, &self.info, meeting, &group);
        }
        self.roster(&group, "");
        Ok(())
    }

    /// Bring every call whose rooms have run out back together. Returns when
    /// to look again.
    fn tick(&mut self, now: Instant) -> Option<Instant> {
        let due: Vec<Vec<u8>> = self
            .meetings
            .iter()
            .filter(|(_, m)| {
                m.rooms
                    .as_ref()
                    .and_then(|r| r.until)
                    .is_some_and(|t| t <= now)
            })
            .map(|(g, _)| g.clone())
            .collect();
        for group in due {
            if let Some(m) = self.meetings.get_mut(&group) {
                close_rooms(&mut self.info, m, &group);
                apply(&mut self.sfu, &self.info, m, &group);
            }
            self.roster(&group, "");
        }
        self.meetings
            .values()
            .filter_map(|m| m.rooms.as_ref().and_then(|r| r.until))
            .min()
    }

    fn signal(&mut self, s: Signal) {
        match s {
            Signal::Offer { to, sdp, tracks } => {
                let Some(i) = self.info.get(&to) else { return };
                let msg = NetMessage::CallOffer {
                    group: i.group.clone(),
                    sdp,
                    tracks: tracks
                        .into_iter()
                        .map(|t| CallTrack {
                            mid: t.mid,
                            participant: t.from.0,
                            kind: match t.kind {
                                Kind::Audio => CallTrackKind::Audio,
                                Kind::Video => CallTrackKind::Camera,
                                Kind::Screen => CallTrackKind::Screen,
                            },
                        })
                        .collect(),
                };
                self.send_to(i.device, &msg);
            }
            Signal::Left { who } => {
                if let Some(i) = self.info.remove(&who) {
                    tracing::info!(participant = who.0, username = %i.username, "left a call");
                    let order = self.sfu.members(&i.group);
                    succeed(&mut self.info, &order, &i.group);
                    if let Some(m) = self.meetings.get_mut(&i.group) {
                        if m.spotlight == Some(who) {
                            m.spotlight = None;
                        }
                    }
                    self.roster(&i.group, "");
                    if !self.info.values().any(|o| o.group == i.group) {
                        // Nobody left to let anyone in: whoever waits is told.
                        if let Some(m) = self.meetings.remove(&i.group) {
                            for w in m.waiting {
                                self.send_to(
                                    w.joiner.device,
                                    &crate::net::call_error(w.joiner.request_id, "the call ended"),
                                );
                            }
                        }
                        self.members.remove(&i.group);
                    }
                }
            }
        }
    }

    /// Who is in a chat's call now, and how its meeting stands.
    fn roster_msg(&self, group: &[u8]) -> NetMessage {
        let meeting = self.meetings.get(group);
        let settings = meeting.map(|m| m.settings).unwrap_or_default();
        let members = self
            .sfu
            .members(group)
            .into_iter()
            .filter_map(|id| {
                let i = self.info.get(&id)?;
                Some(CallMember {
                    participant: id.0,
                    username: i.username.clone(),
                    device: i.device,
                    role: i.role,
                    hand: i.hand,
                    held: i.sound_held(&settings),
                    sharing: i.sharing && !i.screen_held(&settings),
                    muted: i.muted,
                    recording: i.recording,
                    guest: i.guest,
                    room: i.room,
                })
            })
            .collect();
        NetMessage::CallRoster {
            group: group.to_vec(),
            members,
            settings,
            waiting: meeting
                .map(|m| {
                    m.waiting
                        .iter()
                        .map(|w| CallWaiter {
                            participant: w.participant.0,
                            username: w.joiner.username.clone(),
                            guest: w.joiner.guest,
                            since_ms: w.since_ms,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            spotlight: meeting.and_then(|m| m.spotlight).map(|p| p.0),
            rooms: meeting.and_then(|m| m.rooms.as_ref()).map(|r| CallRooms {
                names: r.names.clone(),
                until_ms: r.until_ms,
                may_move: r.may_move,
            }),
        }
    }

    /// Tell every connected device of the chat who is in its call now, on
    /// behalf of `from` — the person who just joined, or nobody for a
    /// departure or a change in the meeting. Through `deliver_private`, so a
    /// blocked person's joining does not ring for the person who blocked them,
    /// while a departure reaches everyone.
    fn roster(&self, group: &[u8], from: &str) {
        let Some(accounts) = self.members.get(group) else {
            return;
        };
        if let Ok(frame) = encode_app_frame(&self.roster_msg(group)) {
            self.chat
                .deliver_private(accounts, from, false, &Bytes::from(frame));
        }
        // A guest is no member of the chat: told directly.
        for i in self.info.values().filter(|i| i.group == group && i.guest) {
            self.send_to(i.device, &self.roster_msg(group));
        }
    }
}

async fn run(socket: UdpSocket, mut state: State, mut rx: mpsc::UnboundedReceiver<Command>) {
    let mut buf = vec![0u8; 2048];
    let mut next = Instant::now();
    loop {
        let wake = tokio::time::Instant::from_std(next);
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return };
                state.command(cmd);
            }
            got = socket.recv_from(&mut buf) => {
                if let Ok((n, from)) = got {
                    state.sfu.receive(Instant::now(), from, &buf[..n]);
                }
            }
            () = tokio::time::sleep_until(wake) => {}
        }

        let now = Instant::now();
        let rooms_due = state.tick(now);
        let (out, at) = state.sfu.poll(now);
        next = rooms_due.map_or(at, |r| r.min(at));
        for t in out {
            // A full send buffer drops the datagram, which is what UDP does
            // anyway; waiting for room would stall every other call.
            let _ = socket.try_send_to(&t.contents, t.destination);
        }
        let signals: Vec<Signal> = state.sfu.signals().collect();
        for s in signals {
            state.signal(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joiner(group: &[u8], name: &str, guest: bool) -> Joiner {
        Joiner {
            group: group.to_vec(),
            username: name.into(),
            account: format!("acct-{name}"),
            device: uuid::Uuid::new_v4(),
            blocked: HashSet::new(),
            members: Vec::new(),
            offer: String::new(),
            screen: None,
            request_id: 1,
            guest,
        }
    }

    fn person(group: &[u8], name: &str, role: CallRole) -> Info {
        Info::new(&joiner(group, name, false), role)
    }

    /// Ann hosts, Bob and Cat are members; Dan is in another call.
    fn meeting() -> (HashMap<ParticipantId, Info>, Meeting) {
        let g = b"g".as_slice();
        let mut info = HashMap::new();
        info.insert(ParticipantId(1), person(g, "ann", CallRole::Host));
        info.insert(ParticipantId(2), person(g, "bob", CallRole::Member));
        info.insert(ParticipantId(3), person(g, "cat", CallRole::Member));
        info.insert(ParticipantId(9), person(b"other", "dan", CallRole::Host));
        (info, Meeting::default())
    }

    const ANN: ParticipantId = ParticipantId(1);
    const BOB: ParticipantId = ParticipantId(2);
    const CAT: ParticipantId = ParticipantId(3);

    fn act(
        info: &mut HashMap<ParticipantId, Info>,
        m: &mut Meeting,
        who: ParticipantId,
        action: CallAction,
    ) -> Result<Outcome, String> {
        control(info, m, who, action, Instant::now(), 1_000)
    }

    #[test]
    fn hands_queue_in_the_order_they_went_up() {
        let (mut info, mut m) = meeting();
        act(&mut info, &mut m, CAT, CallAction::Hand(true)).unwrap();
        act(&mut info, &mut m, BOB, CallAction::Hand(true)).unwrap();
        // A second raise keeps the place in the queue.
        act(&mut info, &mut m, CAT, CallAction::Hand(true)).unwrap();
        assert!(info[&CAT].hand < info[&BOB].hand);
        act(&mut info, &mut m, CAT, CallAction::Hand(false)).unwrap();
        assert_eq!(info[&CAT].hand, None);
    }

    #[test]
    fn only_a_host_acts_on_others() {
        let (mut info, mut m) = meeting();
        let refused = act(&mut info, &mut m, BOB, CallAction::Mute { participant: 3 });
        assert!(refused.is_err());
        assert!(!info[&CAT].held);
        act(&mut info, &mut m, ANN, CallAction::Mute { participant: 3 }).unwrap();
        assert!(info[&CAT].sound_held(&m.settings));
    }

    #[test]
    fn letting_somebody_talk_releases_them_and_lowers_their_hand() {
        let (mut info, mut m) = meeting();
        act(&mut info, &mut m, ANN, CallAction::Mute { participant: 2 }).unwrap();
        act(&mut info, &mut m, BOB, CallAction::Hand(true)).unwrap();
        act(
            &mut info,
            &mut m,
            ANN,
            CallAction::AllowTalk { participant: 2 },
        )
        .unwrap();
        assert!(!info[&BOB].sound_held(&m.settings));
        assert_eq!(info[&BOB].hand, None);
    }

    #[test]
    fn mute_all_spares_the_hosts_and_other_calls() {
        let (mut info, mut m) = meeting();
        act(&mut info, &mut m, ANN, CallAction::MuteAll).unwrap();
        assert!(info[&BOB].held && info[&CAT].held);
        assert!(!info[&ANN].held);
        assert!(!info[&ParticipantId(9)].held);
    }

    #[test]
    fn members_who_may_not_unmute_are_held_until_let_talk() {
        let (mut info, mut m) = meeting();
        let quiet = CallSettings {
            may_unmute: false,
            ..CallSettings::default()
        };
        act(&mut info, &mut m, ANN, CallAction::Settings(quiet)).unwrap();
        assert!(info[&BOB].sound_held(&m.settings));
        assert!(!info[&ANN].sound_held(&m.settings));
        act(
            &mut info,
            &mut m,
            ANN,
            CallAction::AllowTalk { participant: 2 },
        )
        .unwrap();
        assert!(!info[&BOB].sound_held(&m.settings));
        assert!(info[&CAT].sound_held(&m.settings));
    }

    #[test]
    fn a_screen_passes_only_while_sharing_and_allowed() {
        let (mut info, mut m) = meeting();
        assert!(info[&BOB].screen_held(&m.settings));
        act(&mut info, &mut m, BOB, CallAction::Sharing(true)).unwrap();
        assert!(!info[&BOB].screen_held(&m.settings));
        act(
            &mut info,
            &mut m,
            ANN,
            CallAction::StopSharing { participant: 2 },
        )
        .unwrap();
        assert!(info[&BOB].screen_held(&m.settings));
        let hosts_only = CallSettings {
            may_share: false,
            ..CallSettings::default()
        };
        act(&mut info, &mut m, ANN, CallAction::Settings(hosts_only)).unwrap();
        assert!(act(&mut info, &mut m, BOB, CallAction::Sharing(true)).is_err());
        act(&mut info, &mut m, ANN, CallAction::Sharing(true)).unwrap();
        assert!(!info[&ANN].screen_held(&m.settings));
    }

    #[test]
    fn a_removed_person_is_kept_out_and_a_locked_call_lets_back_only_who_was_in() {
        let (mut info, mut m) = meeting();
        m.been_in
            .extend(["acct-ann", "acct-bob", "acct-cat"].map(String::from));
        let out = act(
            &mut info,
            &mut m,
            ANN,
            CallAction::Remove { participant: 3 },
        )
        .unwrap();
        assert_eq!(
            out.leaving,
            vec![(CAT, CallEndReason::Removed { by: "ann".into() })]
        );
        assert!(refuse_join(Some(&m), "acct-cat").is_some());
        let locked = CallSettings {
            locked: true,
            ..CallSettings::default()
        };
        act(&mut info, &mut m, ANN, CallAction::Settings(locked)).unwrap();
        assert!(refuse_join(Some(&m), "acct-bob").is_none());
        assert!(refuse_join(Some(&m), "acct-eve").is_some());
        assert!(refuse_join(None, "acct-eve").is_none());
    }

    #[test]
    fn handing_over_makes_the_old_host_a_co_host_and_nobody_demotes_the_host() {
        let (mut info, mut m) = meeting();
        let co = CallAction::Role {
            participant: 2,
            role: CallRole::CoHost,
        };
        act(&mut info, &mut m, ANN, co).unwrap();
        let demote_host = CallAction::Role {
            participant: 1,
            role: CallRole::Member,
        };
        assert!(act(&mut info, &mut m, BOB, demote_host).is_err());
        let take_over = CallAction::Role {
            participant: 2,
            role: CallRole::Host,
        };
        assert!(act(&mut info, &mut m, BOB, take_over).is_err());
        let hand_over = CallAction::Role {
            participant: 3,
            role: CallRole::Host,
        };
        act(&mut info, &mut m, ANN, hand_over).unwrap();
        assert_eq!(info[&CAT].role, CallRole::Host);
        assert_eq!(info[&ANN].role, CallRole::CoHost);
    }

    #[test]
    fn ending_for_everyone_ends_only_this_call() {
        let (mut info, mut m) = meeting();
        let mut out = act(&mut info, &mut m, ANN, CallAction::EndForAll)
            .unwrap()
            .leaving;
        out.sort_by_key(|(id, _)| *id);
        assert_eq!(
            out.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![ANN, BOB, CAT]
        );
    }

    #[test]
    fn a_call_never_loses_its_host() {
        let (mut info, _) = meeting();
        info.remove(&ANN);
        succeed(&mut info, &[CAT, BOB], b"g");
        assert_eq!(info[&CAT].role, CallRole::Host);
        assert_eq!(info[&BOB].role, CallRole::Member);
        // With a co-host left, nobody is promoted.
        let (mut info, _) = meeting();
        info.get_mut(&BOB).unwrap().role = CallRole::CoHost;
        info.remove(&ANN);
        succeed(&mut info, &[CAT, BOB], b"g");
        assert_eq!(info[&CAT].role, CallRole::Member);
    }

    #[test]
    fn guests_wait_and_members_wait_only_when_the_hosts_say_so() {
        let mut m = Meeting::default();
        let member = joiner(b"g", "eve", false);
        let guest = joiner(b"g", "gus", true);
        // Nobody in yet: nobody waits (and a guest is refused before this).
        assert!(!must_wait(Some(&m), &guest, false));
        assert!(must_wait(Some(&m), &guest, true));
        assert!(must_wait(None, &guest, true));
        assert!(!must_wait(Some(&m), &member, true));
        m.settings.members_wait = true;
        assert!(must_wait(Some(&m), &member, true));
        // Back after a dropped connection: straight in.
        m.been_in.insert("acct-eve".into());
        assert!(!must_wait(Some(&m), &member, true));
    }

    #[test]
    fn a_host_admits_and_denies_only_who_is_waiting() {
        let (mut info, mut m) = meeting();
        m.waiting.push(Waiter {
            participant: ParticipantId(7),
            joiner: joiner(b"g", "gus", true),
            since_ms: 0,
        });
        assert!(act(&mut info, &mut m, BOB, CallAction::Admit { participant: 7 }).is_err());
        assert!(act(&mut info, &mut m, ANN, CallAction::Admit { participant: 8 }).is_err());
        let out = act(&mut info, &mut m, ANN, CallAction::AdmitAll).unwrap();
        assert_eq!(out.admit, vec![ParticipantId(7)]);
        let out = act(&mut info, &mut m, ANN, CallAction::Deny { participant: 7 }).unwrap();
        assert_eq!(out.deny, vec![ParticipantId(7)]);
    }

    #[test]
    fn recording_is_refused_unless_allowed_and_said_either_way() {
        let (mut info, mut m) = meeting();
        assert!(act(&mut info, &mut m, BOB, CallAction::Recording(true)).is_err());
        assert!(!info[&BOB].recording);
        m.settings.may_record = true;
        act(&mut info, &mut m, BOB, CallAction::Recording(true)).unwrap();
        assert!(info[&BOB].recording);
    }

    #[test]
    fn breakout_rooms_split_move_and_close() {
        let (mut info, mut m) = meeting();
        let open = CallAction::OpenRooms {
            names: vec!["Stage".into(), "Food".into()],
            assign: vec![(2, 1), (3, 2)],
            minutes: 10,
            may_move: false,
        };
        assert!(act(&mut info, &mut m, BOB, open.clone()).is_err());
        let out = act(&mut info, &mut m, ANN, open).unwrap();
        assert!(out.rooms_changed);
        assert_eq!(
            (info[&ANN].room, info[&BOB].room, info[&CAT].room),
            (0, 1, 2)
        );
        assert_eq!(m.rooms.as_ref().unwrap().until_ms, Some(1_000 + 600_000));
        // Moving yourself needs the rooms to allow it.
        let me_to_food = CallAction::MoveTo {
            participant: 2,
            room: 2,
        };
        assert!(act(&mut info, &mut m, BOB, me_to_food.clone()).is_err());
        m.rooms.as_mut().unwrap().may_move = true;
        act(&mut info, &mut m, BOB, me_to_food).unwrap();
        assert_eq!(info[&BOB].room, 2);
        assert!(act(
            &mut info,
            &mut m,
            BOB,
            CallAction::MoveTo {
                participant: 2,
                room: 9
            }
        )
        .is_err());
        act(&mut info, &mut m, ANN, CallAction::CloseRooms).unwrap();
        assert!(m.rooms.is_none());
        assert_eq!((info[&BOB].room, info[&CAT].room), (0, 0));
        assert_eq!(info[&ParticipantId(9)].room, 0);
    }

    #[test]
    fn spotlight_names_somebody_in_the_call_or_nobody() {
        let (mut info, mut m) = meeting();
        act(
            &mut info,
            &mut m,
            ANN,
            CallAction::Spotlight {
                participant: Some(3),
            },
        )
        .unwrap();
        assert_eq!(m.spotlight, Some(CAT));
        assert!(act(
            &mut info,
            &mut m,
            ANN,
            CallAction::Spotlight {
                participant: Some(9)
            }
        )
        .is_err());
        act(
            &mut info,
            &mut m,
            ANN,
            CallAction::Spotlight { participant: None },
        )
        .unwrap();
        assert_eq!(m.spotlight, None);
    }
}
