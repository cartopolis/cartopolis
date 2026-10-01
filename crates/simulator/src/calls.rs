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

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use cartopolis_calls::sfu::{Kind, ParticipantId, Sfu, Signal};
use cartopolis_core::{encode_app_frame, CallMember, CallTrack, NetMessage};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

use crate::chat::ChatHub;

/// Longest SDP accepted from a client. An audio-and-video offer is ~3 kB.
pub const MAX_SDP_BYTES: usize = 16 * 1024;

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
}

struct Info {
    group: Vec<u8>,
    username: String,
    device: uuid::Uuid,
    blocked: HashSet<String>,
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
                        match sfu.join(Instant::now(), group.clone(), &joiner.offer) {
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
                                info.insert(id, Info {
                                    group: group.clone(),
                                    username: joiner.username,
                                    device: joiner.device,
                                    blocked: joiner.blocked,
                                });
                                members.insert(group.clone(), joiner.members);
                                // The first one in starts the call: wake the phones of
                                // everyone else in the chat whose app is closed (`push`).
                                if sfu.members(&group).len() == 1 {
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
                                roster(&chat, &sfu, &info, &members, &group, &who);
                            }
                            Err(e) => {
                                let _ = reply.send(Err(e.to_string()));
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
                            let list = roster_of(&sfu, &info, group);
                            // A call made only of people this account blocks
                            // does not ring for it, as `deliver_private` rules
                            // for a live roster.
                            if list.is_empty()
                                || list.iter().all(|m| blocked.contains(&m.username.to_lowercase()))
                            {
                                continue;
                            }
                            let msg = NetMessage::CallRoster { group: group.clone(), members: list };
                            if let Ok(frame) = encode_app_frame(&msg) {
                                chat.deliver_to_device(device, &Bytes::from(frame));
                            }
                        }
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
                                video: t.kind == Kind::Video,
                            })
                            .collect(),
                    };
                    if let Ok(frame) = encode_app_frame(&msg) {
                        chat.deliver_to_device(i.device, &Bytes::from(frame));
                    }
                }
                Signal::Left { who } => {
                    if let Some(i) = info.remove(&who) {
                        tracing::info!(participant = who.0, username = %i.username, "left a call");
                        roster(&chat, &sfu, &info, &members, &i.group, "");
                        if !info.values().any(|o| o.group == i.group) {
                            members.remove(&i.group);
                        }
                    }
                }
            }
        }
    }
}

/// Tell every connected device of the chat who is in its call now, on behalf
/// of `from` — the person who just joined, or nobody for a departure. Through
/// `deliver_private`, so a blocked person's joining does not ring for the
/// person who blocked them, while a departure reaches everyone.
fn roster(
    chat: &ChatHub,
    sfu: &Sfu,
    info: &HashMap<ParticipantId, Info>,
    members: &HashMap<Vec<u8>, Vec<String>>,
    group: &[u8],
    from: &str,
) {
    let Some(accounts) = members.get(group) else {
        return;
    };
    let msg = NetMessage::CallRoster {
        group: group.to_vec(),
        members: roster_of(sfu, info, group),
    };
    if let Ok(frame) = encode_app_frame(&msg) {
        chat.deliver_private(accounts, from, false, &Bytes::from(frame));
    }
}

/// Who is in a chat's call now.
fn roster_of(sfu: &Sfu, info: &HashMap<ParticipantId, Info>, group: &[u8]) -> Vec<CallMember> {
    sfu.members(group)
        .into_iter()
        .filter_map(|id| {
            let i = info.get(&id)?;
            Some(CallMember {
                participant: id.0,
                username: i.username.clone(),
                device: i.device,
            })
        })
        .collect()
}
