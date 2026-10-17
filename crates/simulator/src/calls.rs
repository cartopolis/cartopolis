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
//! raised hands, and settings, and what a host decides is carried out here —
//! held sound and a forbidden screen are dropped at the forwarder
//! ([`Sfu::hold`]), a removed person is taken out and kept out — rather than
//! asked of the other apps, which could be rewritten to ignore it.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use cartopolis_calls::sfu::{Kind, ParticipantId, Sfu, Signal};
use cartopolis_core::{
    encode_app_frame, CallAction, CallEndReason, CallMember, CallRole, CallSettings, CallTrack,
    CallTrackKind, NetMessage,
};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

use crate::chat::ChatHub;

/// Longest SDP accepted from a client. An audio-and-video offer is ~3 kB.
pub const MAX_SDP_BYTES: usize = 16 * 1024;

/// Longest sealed reaction relayed. An emoji, its nonce and its tag are well
/// under a hundred bytes.
pub const MAX_REACTION_BYTES: usize = 512;

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
}

enum Command {
    Join {
        joiner: Joiner,
        reply: oneshot::Sender<Result<(u64, String), String>>,
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
    /// Join a call. On success the seat is the connection's until it drops it.
    pub async fn join(&self, joiner: Joiner) -> Result<(Seat, String), String> {
        let group = joiner.group.clone();
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Join { joiner, reply })
            .map_err(|_| "calls are not running on this server".to_string())?;
        let (participant, answer) = rx
            .await
            .map_err(|_| "calls are not running on this server".to_string())??;
        Ok((
            Seat {
                handle: self.clone(),
                group,
                participant,
            },
            answer,
        ))
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
    /// `participant`. `Err` is
    /// the refusal, for a person.
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

    /// A sealed reaction from this seat, for everyone else in its call.
    pub fn react(&self, seat: &Seat, epoch: u64, sealed: Vec<u8>) {
        let _ = self.tx.send(Command::React {
            participant: seat.participant,
            epoch,
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
}

impl Info {
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

/// Carry out `actor`'s `action` on the call's state. Returns who must leave,
/// and why: a removal, or everybody at an end-for-all.
fn control(
    info: &mut HashMap<ParticipantId, Info>,
    meeting: &mut Meeting,
    actor: ParticipantId,
    action: CallAction,
) -> Result<Vec<(ParticipantId, CallEndReason)>, String> {
    let (group, role, by) = match info.get(&actor) {
        Some(i) => (i.group.clone(), i.role, i.username.clone()),
        None => return Err("you are not in a call".into()),
    };
    let in_call = |p: u64| -> Result<ParticipantId, String> {
        let id = ParticipantId(p);
        match info.get(&id) {
            Some(i) if i.group == group => Ok(id),
            _ => Err("they are no longer in the call".into()),
        }
    };
    match action {
        CallAction::Hand(up) => {
            let me = info.get_mut(&actor).expect("checked above");
            if !up {
                me.hand = None;
            } else if me.hand.is_none() {
                meeting.next_hand += 1;
                me.hand = Some(meeting.next_hand);
            }
            return Ok(Vec::new());
        }
        CallAction::Muted(on) => {
            info.get_mut(&actor).expect("checked above").muted = on;
            return Ok(Vec::new());
        }
        CallAction::Sharing(on) => {
            if on && !meeting.settings.may_share && !role.hosts() {
                return Err("only the hosts may share a screen in this call".into());
            }
            info.get_mut(&actor).expect("checked above").sharing = on;
            return Ok(Vec::new());
        }
        _ => {}
    }
    if !role.hosts() {
        return Err("only a host can do that".into());
    }
    match action {
        CallAction::Hand(_) | CallAction::Sharing(_) | CallAction::Muted(_) => {
            unreachable!("handled above")
        }
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
            return Ok(vec![(id, CallEndReason::Removed { by })]);
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
            return Ok(info
                .iter()
                .filter(|(_, i)| i.group == group)
                .map(|(&id, _)| (id, CallEndReason::Ended { by: by.clone() }))
                .collect());
        }
    }
    Ok(Vec::new())
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

/// Make the forwarder drop what the meeting says is held, for everyone in `group`.
fn apply_holds(
    sfu: &mut Sfu,
    info: &HashMap<ParticipantId, Info>,
    meeting: &Meeting,
    group: &[u8],
) {
    let now = Instant::now();
    for (&id, i) in info.iter().filter(|(_, i)| i.group == group) {
        sfu.hold(id, i.sound_held(&meeting.settings));
        sfu.hold_screen(now, id, i.screen_held(&meeting.settings));
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
    let task = tokio::spawn(run(socket, Sfu::new(public), rx, chat, ring));
    Ok((CallsHandle { tx }, bound, task))
}

fn send_to(chat: &ChatHub, device: uuid::Uuid, msg: &NetMessage) {
    if let Ok(frame) = encode_app_frame(msg) {
        chat.deliver_to_device(device, &Bytes::from(frame));
    }
}

async fn run(
    socket: UdpSocket,
    mut sfu: Sfu,
    mut rx: mpsc::UnboundedReceiver<Command>,
    chat: Arc<ChatHub>,
    ring: Option<mpsc::UnboundedSender<crate::push::Ring>>,
) {
    let mut info: HashMap<ParticipantId, Info> = HashMap::new();
    // Every member account of a chat whose call has anyone in it, for the
    // roster. Refreshed at each join, which is when it is read from the table.
    let mut members: HashMap<Vec<u8>, Vec<String>> = HashMap::new();
    let mut meetings: HashMap<Vec<u8>, Meeting> = HashMap::new();
    let mut buf = vec![0u8; 2048];
    let mut next = Instant::now();
    loop {
        let wake = tokio::time::Instant::from_std(next);
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return };
                match cmd {
                    Command::Join { joiner, reply } => {
                        let group = joiner.group.clone();
                        let joined = match refuse_join(meetings.get(&group), &joiner.account) {
                            Some(why) => Err(why.to_string()),
                            None => sfu
                                .join(Instant::now(), group.clone(), &joiner.offer, joiner.screen.as_deref())
                                .map_err(|e| e.to_string()),
                        };
                        match joined {
                            Ok((id, answer)) => {
                                for (&other, o) in info.iter().filter(|(_, o)| o.group == group) {
                                    if joiner.blocked.contains(&o.username.to_lowercase()) {
                                        sfu.ignore(id, other);
                                    }
                                    if o.blocked.contains(&joiner.username.to_lowercase()) {
                                        sfu.ignore(other, id);
                                    }
                                }
                                tracing::info!(participant = id.0, username = %joiner.username, "joined a call");
                                // The first one in hosts it.
                                let first = sfu.members(&group).len() == 1;
                                info.insert(id, Info {
                                    group: group.clone(),
                                    username: joiner.username,
                                    account: joiner.account.clone(),
                                    device: joiner.device,
                                    blocked: joiner.blocked,
                                    role: if first { CallRole::Host } else { CallRole::Member },
                                    hand: None,
                                    held: false,
                                    allowed: false,
                                    sharing: false,
                                    muted: false,
                                });
                                members.insert(group.clone(), joiner.members);
                                let meeting = meetings.entry(group.clone()).or_default();
                                meeting.been_in.insert(joiner.account.clone());
                                apply_holds(&mut sfu, &info, meeting, &group);
                                // The first one in starts the call: wake the phones of
                                // everyone else in the chat whose app is closed (`push`).
                                if first {
                                    if let Some(ring) = &ring {
                                        let _ = ring.send(crate::push::Ring {
                                            caller: joiner.account.clone(),
                                            caller_name: info[&id].username.clone(),
                                            members: members[&group].clone(),
                                        });
                                    }
                                }
                                let _ = reply.send(Ok((id.0, answer)));
                                let who = info[&id].username.clone();
                                roster(&chat, &sfu, &info, &members, &meetings, &group, &who);
                            }
                            Err(e) => {
                                let _ = reply.send(Err(e));
                            }
                        }
                    }
                    Command::Answer { participant, sdp } => {
                        if let Err(e) = sfu.answer(Instant::now(), ParticipantId(participant), &sdp) {
                            tracing::debug!(participant, "call answer refused: {e}");
                        }
                    }
                    Command::Leave { participant } => sfu.leave(ParticipantId(participant)),
                    Command::Catchup { account, device, blocked } => {
                        for (group, accounts) in &members {
                            if !accounts.contains(&account) {
                                continue;
                            }
                            let list = roster_of(&sfu, &info, &meetings, group);
                            // A call made only of people this account blocks
                            // does not ring for it, as `deliver_private` rules
                            // for a live roster.
                            if list.is_empty()
                                || list.iter().all(|m| blocked.contains(&m.username.to_lowercase()))
                            {
                                continue;
                            }
                            let msg = NetMessage::CallRoster {
                                group: group.clone(),
                                members: list,
                                settings: meetings.get(group).map(|m| m.settings).unwrap_or_default(),
                            };
                            send_to(&chat, device, &msg);
                        }
                    }
                    Command::Control { participant, action, reply } => {
                        let actor = ParticipantId(participant);
                        let group = info.get(&actor).map(|i| i.group.clone()).unwrap_or_default();
                        let meeting = meetings.entry(group.clone()).or_default();
                        match control(&mut info, meeting, actor, action) {
                            Ok(leaving) => {
                                let _ = reply.send(Ok(()));
                                for (id, reason) in leaving {
                                    if let Some(i) = info.get(&id) {
                                        send_to(&chat, i.device, &NetMessage::CallEnded {
                                            group: group.clone(),
                                            reason,
                                        });
                                    }
                                    sfu.leave(id);
                                }
                                apply_holds(&mut sfu, &info, meeting, &group);
                                roster(&chat, &sfu, &info, &members, &meetings, &group, "");
                            }
                            Err(why) => {
                                let _ = reply.send(Err(why));
                            }
                        }
                    }
                    Command::React { participant, epoch, sealed } => {
                        let from = ParticipantId(participant);
                        if let Some(sender) = info.get(&from) {
                            let msg = NetMessage::CallReacted {
                                group: sender.group.clone(),
                                participant,
                                epoch,
                                sealed,
                            };
                            let name = sender.username.to_lowercase();
                            for (_, o) in info.iter().filter(|(&id, o)| {
                                id != from && o.group == sender.group && !o.blocked.contains(&name)
                            }) {
                                send_to(&chat, o.device, &msg);
                            }
                        }
                    }
                    Command::Watch { participant, cameras } => {
                        sfu.watch(
                            Instant::now(),
                            ParticipantId(participant),
                            cameras.map(|c| c.into_iter().map(ParticipantId).collect()),
                        );
                    }
                }
            }
            got = socket.recv_from(&mut buf) => {
                if let Ok((n, from)) = got {
                    sfu.receive(Instant::now(), from, &buf[..n]);
                }
            }
            () = tokio::time::sleep_until(wake) => {}
        }

        let (out, at) = sfu.poll(Instant::now());
        next = at;
        for t in out {
            // A full send buffer drops the datagram, which is what UDP does
            // anyway; waiting for room would stall every other call.
            let _ = socket.try_send_to(&t.contents, t.destination);
        }
        let signals: Vec<Signal> = sfu.signals().collect();
        for s in signals {
            match s {
                Signal::Offer { to, sdp, tracks } => {
                    let Some(i) = info.get(&to) else { continue };
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
                    send_to(&chat, i.device, &msg);
                }
                Signal::Left { who } => {
                    if let Some(i) = info.remove(&who) {
                        tracing::info!(participant = who.0, username = %i.username, "left a call");
                        succeed(&mut info, &sfu.members(&i.group), &i.group);
                        roster(&chat, &sfu, &info, &members, &meetings, &i.group, "");
                        if !info.values().any(|o| o.group == i.group) {
                            members.remove(&i.group);
                            meetings.remove(&i.group);
                        }
                    }
                }
            }
        }
    }
}

/// Tell every connected device of the chat who is in its call now, on behalf
/// of `from` — the person who just joined, or nobody for a departure or a
/// change in the meeting. Through `deliver_private`, so a blocked person's
/// joining does not ring for the person who blocked them, while a departure
/// reaches everyone.
fn roster(
    chat: &ChatHub,
    sfu: &Sfu,
    info: &HashMap<ParticipantId, Info>,
    members: &HashMap<Vec<u8>, Vec<String>>,
    meetings: &HashMap<Vec<u8>, Meeting>,
    group: &[u8],
    from: &str,
) {
    let Some(accounts) = members.get(group) else {
        return;
    };
    let msg = NetMessage::CallRoster {
        group: group.to_vec(),
        members: roster_of(sfu, info, meetings, group),
        settings: meetings.get(group).map(|m| m.settings).unwrap_or_default(),
    };
    if let Ok(frame) = encode_app_frame(&msg) {
        chat.deliver_private(accounts, from, false, &Bytes::from(frame));
    }
}

/// Who is in a chat's call now.
fn roster_of(
    sfu: &Sfu,
    info: &HashMap<ParticipantId, Info>,
    meetings: &HashMap<Vec<u8>, Meeting>,
    group: &[u8],
) -> Vec<CallMember> {
    let settings = meetings.get(group).map(|m| m.settings).unwrap_or_default();
    sfu.members(group)
        .into_iter()
        .filter_map(|id| {
            let i = info.get(&id)?;
            Some(CallMember {
                participant: id.0,
                username: i.username.clone(),
                device: i.device,
                role: i.role,
                hand: i.hand,
                held: i.sound_held(&settings),
                sharing: i.sharing && !i.screen_held(&settings),
                muted: i.muted,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(group: &[u8], name: &str, role: CallRole) -> Info {
        Info {
            group: group.to_vec(),
            username: name.into(),
            account: format!("acct-{name}"),
            device: uuid::Uuid::new_v4(),
            blocked: HashSet::new(),
            role,
            hand: None,
            held: false,
            allowed: false,
            sharing: false,
            muted: false,
        }
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

    #[test]
    fn hands_queue_in_the_order_they_went_up() {
        let (mut info, mut m) = meeting();
        control(&mut info, &mut m, CAT, CallAction::Hand(true)).unwrap();
        control(&mut info, &mut m, BOB, CallAction::Hand(true)).unwrap();
        // A second raise keeps the place in the queue.
        control(&mut info, &mut m, CAT, CallAction::Hand(true)).unwrap();
        assert!(info[&CAT].hand < info[&BOB].hand);
        control(&mut info, &mut m, CAT, CallAction::Hand(false)).unwrap();
        assert_eq!(info[&CAT].hand, None);
    }

    #[test]
    fn only_a_host_acts_on_others() {
        let (mut info, mut m) = meeting();
        let refused = control(&mut info, &mut m, BOB, CallAction::Mute { participant: 3 });
        assert!(refused.is_err());
        assert!(!info[&CAT].held);
        control(&mut info, &mut m, ANN, CallAction::Mute { participant: 3 }).unwrap();
        assert!(info[&CAT].sound_held(&m.settings));
    }

    #[test]
    fn letting_somebody_talk_releases_them_and_lowers_their_hand() {
        let (mut info, mut m) = meeting();
        control(&mut info, &mut m, ANN, CallAction::Mute { participant: 2 }).unwrap();
        control(&mut info, &mut m, BOB, CallAction::Hand(true)).unwrap();
        control(
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
        control(&mut info, &mut m, ANN, CallAction::MuteAll).unwrap();
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
        control(&mut info, &mut m, ANN, CallAction::Settings(quiet)).unwrap();
        assert!(info[&BOB].sound_held(&m.settings));
        assert!(!info[&ANN].sound_held(&m.settings));
        control(
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
        control(&mut info, &mut m, BOB, CallAction::Sharing(true)).unwrap();
        assert!(!info[&BOB].screen_held(&m.settings));
        control(
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
        control(&mut info, &mut m, ANN, CallAction::Settings(hosts_only)).unwrap();
        assert!(control(&mut info, &mut m, BOB, CallAction::Sharing(true)).is_err());
        control(&mut info, &mut m, ANN, CallAction::Sharing(true)).unwrap();
        assert!(!info[&ANN].screen_held(&m.settings));
    }

    #[test]
    fn a_removed_person_is_kept_out_and_a_locked_call_lets_back_only_who_was_in() {
        let (mut info, mut m) = meeting();
        m.been_in
            .extend(["acct-ann", "acct-bob", "acct-cat"].map(String::from));
        let out = control(
            &mut info,
            &mut m,
            ANN,
            CallAction::Remove { participant: 3 },
        )
        .unwrap();
        assert_eq!(
            out,
            vec![(CAT, CallEndReason::Removed { by: "ann".into() })]
        );
        assert!(refuse_join(Some(&m), "acct-cat").is_some());
        let locked = CallSettings {
            locked: true,
            ..CallSettings::default()
        };
        control(&mut info, &mut m, ANN, CallAction::Settings(locked)).unwrap();
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
        control(&mut info, &mut m, ANN, co).unwrap();
        let demote_host = CallAction::Role {
            participant: 1,
            role: CallRole::Member,
        };
        assert!(control(&mut info, &mut m, BOB, demote_host).is_err());
        let take_over = CallAction::Role {
            participant: 2,
            role: CallRole::Host,
        };
        assert!(control(&mut info, &mut m, BOB, take_over.clone()).is_err());
        let hand_over = CallAction::Role {
            participant: 3,
            role: CallRole::Host,
        };
        control(&mut info, &mut m, ANN, hand_over).unwrap();
        assert_eq!(info[&CAT].role, CallRole::Host);
        assert_eq!(info[&ANN].role, CallRole::CoHost);
    }

    #[test]
    fn ending_for_everyone_ends_only_this_call() {
        let (mut info, mut m) = meeting();
        let mut out = control(&mut info, &mut m, ANN, CallAction::EndForAll).unwrap();
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
}
