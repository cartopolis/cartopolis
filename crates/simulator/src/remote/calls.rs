//! Calls in a chat another server is the hub of
//! (`docs/notes/chat-across-servers.md`, "Calls").
//!
//! The hub's forwarder carries the call for every member, wherever they live,
//! so one call has one forwarder and nobody's media is relayed twice. A
//! device's home forwards what the device says about the call to the hub —
//! its join (`/call/join`), its answers to the forwarder's offers
//! (`/call/answer`) and its leaving (`/call/leave`) — and the hub sends back
//! what its calls task has for that device, or for the members who live there
//! (`/call/signal`): the forwarder's offers, the roster that rings them. The
//! media goes from the device straight to the hub's forwarder, whose address
//! is the one candidate in the forwarder's SDP. The keys are the chat's, so
//! neither server can open a frame, exactly as with one server.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use cartopolis_core::{address, NetMessage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{b64, post, Done, Link};
use crate::chat::ChatHub;
use crate::db::SharedConn;

// ── what servers send each other ────────────────────────────────────────────

/// `who`'s `device` joins the call of `group`, a chat the receiving server is
/// the hub of: a `CallJoin`, forwarded by their home.
#[derive(Debug, Serialize, Deserialize)]
pub struct Join {
    pub who: String,
    pub device: Uuid,
    #[serde(with = "b64")]
    pub group: Vec<u8>,
    pub offer: String,
    pub screen: Option<String>,
    pub request_id: u32,
    /// The people `who` blocks, as full addresses: the forwarder sends them
    /// nothing of theirs. Kept by the home, which is the only one that knows.
    pub blocked: Vec<String>,
}

/// The hub's answer to a [`Join`]: in, with the forwarder's answer; waiting to
/// be let in (`answer` empty); or why not.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Joined {
    pub participant: Option<u64>,
    pub answer: Option<String>,
    pub refused: Option<String>,
}

/// The answer `who`'s `device` gave the forwarder's last offer.
#[derive(Debug, Serialize, Deserialize)]
pub struct Answer {
    pub who: String,
    pub device: Uuid,
    #[serde(with = "b64")]
    pub group: Vec<u8>,
    pub sdp: String,
}

/// `who`'s `device` left the call, or its connection to its home ended.
#[derive(Debug, Serialize, Deserialize)]
pub struct Leave {
    pub who: String,
    pub device: Uuid,
    #[serde(with = "b64")]
    pub group: Vec<u8>,
}

/// A message of the hub's calls task for the receiving server's people: for
/// one `device` in the call, or for every connected device of the people
/// named in `to` (names on the receiving server). Names inside `frame`, and
/// `from`, are as the hub knows them — bare for the hub's own people — and
/// the receiving server writes them as its devices expect.
#[derive(Debug, Serialize, Deserialize)]
pub struct Signal {
    pub device: Option<Uuid>,
    pub to: Vec<String>,
    /// Whose doing it is (who just joined), or empty: somebody who blocked
    /// them is not rung by it.
    pub from: String,
    #[serde(with = "b64")]
    pub frame: Vec<u8>,
}

// ── the hub's side ──────────────────────────────────────────────────────────

/// A member of a chat who lives on another server.
#[derive(Debug, Clone)]
pub struct Member {
    /// The account here that stands for them.
    pub account: String,
    /// Their server.
    pub home: String,
    /// Their name there.
    pub name: String,
}

/// Those of `members` (`accounts.id`) who live elsewhere.
pub fn members_elsewhere(c: &Connection, members: &[String]) -> Vec<Member> {
    members
        .iter()
        .filter_map(|account| {
            let (username, home): (String, Option<String>) = c
                .query_row(
                    "SELECT username, home FROM accounts WHERE id = ?1",
                    params![account],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .ok()??;
            let (name, _) = address::split(&username)?;
            Some(Member {
                account: account.clone(),
                home: home?,
                name: name.to_string(),
            })
        })
        .collect()
}

/// A [`Signal`] for `home`, from the hub's calls task.
pub struct Relay {
    pub home: String,
    pub signal: Signal,
}

/// The task that posts the calls task's [`Relay`]s, in the order they were
/// made: an offer must not overtake the roster before it. One at a time, so
/// a slow home delays the signalling of the others behind it — never the
/// media, which does not pass here. `None` where this server talks to no
/// other.
#[must_use]
pub fn spawn_relay(link: Link, conn: SharedConn) -> Option<mpsc::UnboundedSender<Relay>> {
    if !link.enabled {
        return None;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<Relay>();
    tokio::spawn(async move {
        while let Some(r) = rx.recv().await {
            if let Err(e) = post::<_, Done>(&link, &conn, &r.home, "/call/signal", &r.signal).await
            {
                tracing::info!(home = %r.home, "call elsewhere: not delivered: {e}");
            }
        }
    });
    Some(tx)
}

/// What both sides keep about calls across servers. On [`ChatHub`], the one
/// thing the connections, the HTTP routes and the calls task all share.
#[derive(Default)]
pub struct Elsewhere {
    /// The hub's side: the seats it holds for devices elsewhere, with their
    /// server. Dropping one leaves the call.
    seats: Mutex<HashMap<Uuid, (String, crate::calls::Seat)>>,
    /// The home's side: the devices here in a call another server is the hub of.
    away: Mutex<HashMap<Uuid, Away>>,
    next: AtomicU64,
}

struct Away {
    hub: String,
    group: Vec<u8>,
    /// Which join this is, so the end of an earlier one does not forget it.
    id: u64,
    /// What the hub sent before its answer to the join reached the device,
    /// held until it has: an offer for the receiving connection must not
    /// arrive before the `CallJoined` it follows. `None` once delivered.
    held: Option<Vec<Bytes>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Elsewhere {
    /// Keep `seat` for `device` on `server`; one call per device, so a seat it
    /// held already is given up.
    pub fn keep(&self, device: Uuid, server: &str, seat: crate::calls::Seat) {
        lock(&self.seats).insert(device, (server.to_lowercase(), seat));
    }

    /// The forwarder gets the answer of `device`, if `server` holds its seat
    /// in `group`'s call.
    pub fn answer(
        &self,
        handle: &crate::calls::CallsHandle,
        device: Uuid,
        server: &str,
        group: &[u8],
        sdp: String,
    ) {
        if let Some((s, seat)) = lock(&self.seats).get(&device) {
            if s.eq_ignore_ascii_case(server) && seat.group == group {
                handle.answer(seat, sdp);
            }
        }
    }

    /// `device` on `server` leaves `group`'s call.
    pub fn leave(&self, device: Uuid, server: &str, group: &[u8]) {
        let mut seats = lock(&self.seats);
        if seats
            .get(&device)
            .is_some_and(|(s, seat)| s.eq_ignore_ascii_case(server) && seat.group == group)
        {
            seats.remove(&device);
        }
    }

    /// Hand a frame from `hub` to `device` here, if it is in a call of that
    /// hub's (and in `group`'s, when the message names one). Held while its
    /// join is still being answered.
    pub fn to_device(
        &self,
        chat: &ChatHub,
        hub: &str,
        device: Uuid,
        group: Option<&[u8]>,
        frame: Bytes,
    ) -> bool {
        let mut away = lock(&self.away);
        let Some(a) = away.get_mut(&device) else {
            return false;
        };
        if !a.hub.eq_ignore_ascii_case(hub) || group.is_some_and(|g| g != a.group) {
            return false;
        }
        match &mut a.held {
            Some(held) => held.push(frame),
            None => chat.deliver_to_device(device, &frame),
        }
        true
    }

    /// The device has its `CallJoined` (or `CallWaiting`): what was held for
    /// it follows.
    pub fn release(&self, chat: &ChatHub, device: Uuid) {
        let mut away = lock(&self.away);
        if let Some(held) = away.get_mut(&device).and_then(|a| a.held.take()) {
            for frame in held {
                chat.deliver_to_device(device, &frame);
            }
        }
    }
}

/// The home's side of one device's call on another server's forwarder: what
/// it answers goes to the hub in order, and dropping it — leaving, joining
/// another, or the connection ending — tells the hub it left.
pub struct AwayCall {
    pub group: Vec<u8>,
    tx: mpsc::UnboundedSender<String>,
}

impl AwayCall {
    /// Start one, before the join is sent: whatever the hub signals from then
    /// on is held for the device until [`Elsewhere::release`].
    #[must_use]
    pub fn start(
        link: &Link,
        conn: &SharedConn,
        chat: &Arc<ChatHub>,
        hub: &str,
        who: &str,
        device: Uuid,
        group: &[u8],
    ) -> Self {
        let id = chat.elsewhere.next.fetch_add(1, Ordering::Relaxed);
        lock(&chat.elsewhere.away).insert(
            device,
            Away {
                hub: hub.to_lowercase(),
                group: group.to_vec(),
                id,
                held: Some(Vec::new()),
            },
        );
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (link, conn, chat) = (link.clone(), conn.clone(), chat.clone());
        let (hub, who, group_id) = (hub.to_string(), who.to_string(), group.to_vec());
        tokio::spawn(async move {
            while let Some(sdp) = rx.recv().await {
                let body = Answer {
                    who: who.clone(),
                    device,
                    group: group_id.clone(),
                    sdp,
                };
                if let Err(e) = post::<_, Done>(&link, &conn, &hub, "/call/answer", &body).await {
                    tracing::info!(%hub, "call elsewhere: answer not delivered: {e}");
                }
            }
            {
                let mut away = lock(&chat.elsewhere.away);
                if away.get(&device).is_some_and(|a| a.id == id) {
                    away.remove(&device);
                }
            }
            let body = Leave {
                who,
                device,
                group: group_id,
            };
            if let Err(e) = post::<_, Done>(&link, &conn, &hub, "/call/leave", &body).await {
                tracing::info!(%hub, "call elsewhere: leaving not delivered: {e}");
            }
        });
        Self {
            group: group.to_vec(),
            tx,
        }
    }

    /// The device's answer to the forwarder's last offer.
    pub fn answer(&self, sdp: String) {
        let _ = self.tx.send(sdp);
    }
}

/// The call message in a [`Signal`]'s frame, with the group it is about, or
/// `None` for anything a hub has no business sending. A `ServerError` names
/// no group: it can only be for a device in one of that hub's calls.
#[must_use]
pub fn call_message(frame: &[u8]) -> Option<(NetMessage, Option<Vec<u8>>)> {
    let msg = cartopolis_core::decode_app_frame(frame).ok()?;
    let group = match &msg {
        NetMessage::CallRoster { group, .. }
        | NetMessage::CallOffer { group, .. }
        | NetMessage::CallJoined { group, .. }
        | NetMessage::CallEnded { group, .. }
        | NetMessage::CallReacted { group, .. }
        | NetMessage::CallKeyDropped { group, .. } => Some(group.clone()),
        NetMessage::ServerError { .. } => None,
        _ => return None,
    };
    Some((msg, group))
}

/// Write the names in a message from `hub` as a device on `here` expects
/// them: bare for people here, full addresses for everyone else.
pub fn localise(msg: &mut NetMessage, hub: &str, here: &str) {
    let name = |u: &mut String| {
        let full = address::qualify(u, hub);
        *u = address::display(&full, here).to_string();
    };
    match msg {
        NetMessage::CallRoster {
            members, waiting, ..
        } => {
            members.iter_mut().for_each(|m| name(&mut m.username));
            waiting.iter_mut().for_each(|w| name(&mut w.username));
        }
        NetMessage::CallEnded {
            reason:
                cartopolis_core::CallEndReason::Removed { by }
                | cartopolis_core::CallEndReason::Ended { by },
            ..
        } => name(by),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::{CallMember, CallRole, CallSettings};

    #[test]
    fn a_roster_from_the_hub_names_people_as_home_does() {
        let member = |username: &str| CallMember {
            participant: 1,
            username: username.into(),
            device: Uuid::nil(),
            role: CallRole::Member,
            hand: None,
            held: false,
            sharing: false,
            muted: false,
            recording: false,
            guest: false,
            room: 0,
            agent: false,
        };
        let mut msg = NetMessage::CallRoster {
            group: address::group_id("hub.example", [0; 16]),
            members: vec![member("alice"), member("bob@home.example")],
            settings: CallSettings::default(),
            waiting: Vec::new(),
            spotlight: None,
            rooms: None,
        };
        localise(&mut msg, "hub.example", "home.example");
        let NetMessage::CallRoster { members, .. } = msg else {
            unreachable!()
        };
        let names: Vec<_> = members.iter().map(|m| m.username.as_str()).collect();
        assert_eq!(names, ["alice@hub.example", "bob"]);
    }

    #[test]
    fn a_hub_may_send_call_messages_only() {
        let frame = |m: &NetMessage| cartopolis_core::encode_app_frame(m).unwrap();
        let group = address::group_id("hub.example", [0; 16]);
        let (_, g) = call_message(&frame(&NetMessage::CallEnded {
            group: group.clone(),
            reason: cartopolis_core::CallEndReason::Removed { by: "alice".into() },
        }))
        .unwrap();
        assert_eq!(g, Some(group.clone()));
        assert!(call_message(&frame(&NetMessage::E2eeRedacted {
            group,
            seq: 1,
            by: "x".into(),
        }))
        .is_none());
    }
}
