//! Private chats on the client: this installation's encryption keys, the
//! conversations they open, and the glue to the chat panel
//! (`docs/notes/encrypted-chat.md`, protocol 25).
//!
//! Two layers, split for the tests' sake. [`Engine`] is plain logic — protocol
//! messages in, protocol messages and [`Event`]s out, no Bevy, no I/O — so the
//! suite at the bottom runs two of them against a **real simulator** over
//! loopback and reads back what one typed on the other. [`drive_private_chats`]
//! is the Bevy half: it hands the engine what the network mailbox collected,
//! sends what it answers, turns its events into chat lines, and saves its state.
//!
//! # What lives where
//!
//! The keys never leave the device. They are saved in the **Data** namespace of
//! `platform::storage` (never the evictable cache), one blob per server and
//! account ([`state_key`]). Losing that blob loses every private chat on this
//! device: the server holds only sealed bytes, and the backup that would bring
//! them back is the next step, not this one.
//!
//! # What is deliberately not here yet
//!
//! - **Old chats on a new device.** A second phone joins chats started after it
//!   registered; earlier ones need the encrypted backup (recovery key).
//! - **Groups.** The engine and the server both support more than two people;
//!   only direct chats have a way to be started from the UI so far.
//! - **A padlock.** The chat list says "private" in words until the icon font
//!   gains a lock glyph with the Chats | World interface.

use std::collections::BTreeMap;

use bevy::prelude::*;
use cartopolis_core::{E2eeCursor, E2eeWelcome, NetMessage};
use cartopolis_e2ee::{Device, Payload, Received};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Key packages a device tries to keep on the server. Each lets one person add
/// this device to one chat; the stock is topped up on every connect and
/// whenever the server says it has run low.
const KEY_PACKAGE_STOCK: u32 = 20;

/// Below this many, top up at once rather than waiting for the next connect.
const KEY_PACKAGE_LOW: u32 = 5;

/// Request ids for private-chat traffic live in their own range, so a refusal
/// (`ServerError`) can be told apart from one about a prim edit — the network
/// system deselects the prim on a 403, which is the wrong answer to "that
/// device is registered to another account".
const REQUEST_BASE: u32 = 0xE2EE_0000;

/// Whether a request id belongs to private chats.
#[must_use]
pub fn is_private_request(request_id: u32) -> bool {
    request_id & 0xFFFF_0000 == REQUEST_BASE
}

/// Where a device's state is kept: per server and per account, because the same
/// person on two servers is two unrelated sets of keys.
///
/// On the web this prefix is preloaded before the app starts
/// (`crate::run`), since IndexedDB has no synchronous read.
pub const STATE_PREFIX: &str = "e2ee/";

#[must_use]
pub fn state_key(server: &str, username: &str) -> String {
    // Letters, digits and `-` only. Not `.`: a server address of `..` would
    // otherwise become a key segment that climbs out of the prefix, since the
    // native store turns each `/`-separated segment into a path component.
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
    format!(
        "{STATE_PREFIX}{}/{}.bin",
        clean(server),
        clean(&username.to_lowercase())
    )
}

/// The chat-panel channel key for a private chat.
#[must_use]
pub fn channel_key(group: &[u8]) -> String {
    let mut s = String::from("dm:");
    for b in group {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The group id a private channel key names, if it is one.
#[must_use]
pub fn group_of(channel: &str) -> Option<Vec<u8>> {
    let hex = channel.strip_prefix("dm:")?;
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// What the engine tells the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A private chat is ready to use (just started, or just joined).
    Opened { group: Vec<u8>, peer: String },
    /// Somebody said something in one.
    Line {
        group: Vec<u8>,
        from: String,
        body: String,
        sent_at_ms: u64,
    },
    /// Something the person should be told, in words.
    Notice(String),
}

/// A direct chat being set up: which step it is waiting on.
#[derive(Debug)]
enum Stage {
    /// Waiting for the other person's key packages.
    Claiming,
    /// Waiting for the server to register the chat's id.
    Creating {
        group: Vec<u8>,
        packages: Vec<cartopolis_core::E2eeKeyPackage>,
    },
    /// Waiting for the server to accept the commit that adds them.
    Committing { group: Vec<u8> },
}

#[derive(Debug)]
struct Starting {
    request: u32,
    peer: String,
    stage: Stage,
}

#[derive(Serialize, Deserialize)]
struct SavedEngine {
    device: Vec<u8>,
    cursors: BTreeMap<Vec<u8>, u64>,
    peers: BTreeMap<Vec<u8>, String>,
}

/// One device's private chats, as a state machine over protocol messages.
pub struct Engine {
    device: Device,
    /// Per chat: the last log entry this device has applied. Entries are applied
    /// strictly in order; a gap means something was missed, and the answer is to
    /// ask again rather than to skip ahead (a skipped commit breaks the chat).
    cursors: BTreeMap<Vec<u8>, u64>,
    /// Per chat: who it is with, for its label.
    peers: BTreeMap<Vec<u8>, String>,
    starting: Vec<Starting>,
    next_request: u32,
}

impl Engine {
    /// A brand-new device for `username`.
    pub fn new(username: &str) -> Result<Self, String> {
        Ok(Self {
            device: Device::new(username, Uuid::new_v4()).map_err(|e| e.to_string())?,
            cursors: BTreeMap::new(),
            peers: BTreeMap::new(),
            starting: Vec::new(),
            next_request: 1,
        })
    }

    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        let saved: SavedEngine = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
        Ok(Self {
            device: Device::load(&saved.device).map_err(|e| e.to_string())?,
            cursors: saved.cursors,
            peers: saved.peers,
            starting: Vec::new(),
            next_request: 1,
        })
    }

    /// Everything to keep, including private keys. See the module note on where.
    #[must_use]
    pub fn save(&self) -> Vec<u8> {
        postcard::to_allocvec(&SavedEngine {
            device: self.device.save(),
            cursors: self.cursors.clone(),
            peers: self.peers.clone(),
        })
        .unwrap_or_default()
    }

    #[must_use]
    pub fn username(&self) -> &str {
        &self.device.identity().username
    }

    /// Chats this device is in, with who each is with.
    pub fn chats(&self) -> impl Iterator<Item = (&Vec<u8>, &String)> {
        self.peers.iter()
    }

    fn request(&mut self) -> u32 {
        let id = REQUEST_BASE | (self.next_request & 0xFFFF);
        self.next_request = self.next_request.wrapping_add(1);
        id
    }

    fn key_packages(&mut self, n: u32) -> Vec<Vec<u8>> {
        self.device.key_packages(n as usize).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not make key packages");
            Vec::new()
        })
    }

    fn sync_request(&mut self, only: Option<&[u8]>) -> NetMessage {
        let cursors = self
            .cursors
            .iter()
            .filter(|(g, _)| only.is_none_or(|o| o == g.as_slice()))
            .map(|(g, after)| E2eeCursor {
                group: g.clone(),
                after: *after,
            })
            .collect();
        NetMessage::E2eeSync {
            request_id: self.request(),
            cursors,
        }
    }

    /// Just connected (or reconnected): register this device with a fresh stock
    /// of key packages, and ask for everything missed.
    pub fn on_connected(&mut self, out: &mut Vec<NetMessage>) {
        let key_packages = self.key_packages(KEY_PACKAGE_STOCK);
        out.push(NetMessage::E2eeRegisterDevice {
            request_id: self.request(),
            device: self.device.identity().device,
            key_packages,
        });
        let sync = self.sync_request(None);
        out.push(sync);
        // Whatever was half set up belonged to the previous connection's requests.
        self.starting.clear();
    }

    /// Start (or find) a direct chat with `username`.
    pub fn start_direct(
        &mut self,
        username: &str,
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) {
        let username = username.trim().to_string();
        if username.is_empty() || username.eq_ignore_ascii_case(self.username()) {
            return;
        }
        if let Some((g, _)) = self
            .peers
            .iter()
            .find(|(_, p)| p.eq_ignore_ascii_case(&username))
        {
            events.push(Event::Opened {
                group: g.clone(),
                peer: username,
            });
            return;
        }
        let request = self.request();
        out.push(NetMessage::E2eeClaimKeyPackages {
            request_id: request,
            username: username.clone(),
        });
        self.starting.push(Starting {
            request,
            peer: username,
            stage: Stage::Claiming,
        });
    }

    /// Seal and send a line. The caller shows it straight away: this device
    /// cannot read its own entry back when the server replays it.
    pub fn send_text(
        &mut self,
        group: &[u8],
        body: &str,
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<(), String> {
        let sealed = self
            .device
            .encrypt(
                group,
                &Payload::Text {
                    body: body.to_string(),
                    sent_at_ms: now_ms,
                },
            )
            .map_err(|e| e.to_string())?;
        out.push(NetMessage::E2eeSend {
            request_id: self.request(),
            group: group.to_vec(),
            epoch: sealed.epoch,
            commit: false,
            message: sealed.message,
            welcome: None,
        });
        Ok(())
    }

    /// Apply one message from the server. Anything that is not private-chat
    /// traffic is ignored.
    pub fn handle(&mut self, msg: NetMessage, out: &mut Vec<NetMessage>, events: &mut Vec<Event>) {
        match msg {
            NetMessage::E2eeDeviceStatus { key_packages, .. } => {
                if key_packages < KEY_PACKAGE_LOW {
                    let more = self.key_packages(KEY_PACKAGE_STOCK - key_packages);
                    out.push(NetMessage::E2eeRegisterDevice {
                        request_id: self.request(),
                        device: self.device.identity().device,
                        key_packages: more,
                    });
                }
            }
            NetMessage::E2eeKeyPackages {
                request_id,
                packages,
                ..
            } => self.on_key_packages(request_id, packages, out, events),
            NetMessage::E2eeGroupCreated {
                request_id,
                group,
                existing,
            } => self.on_group_created(request_id, group, existing, out, events),
            NetMessage::E2eeSent {
                request_id,
                group,
                seq,
                refused,
            } => self.on_sent(request_id, &group, seq, refused, events),
            NetMessage::E2eeInvite {
                group,
                from,
                after,
                welcome,
            } => self.on_invite(group, from, after, &welcome, out, events),
            NetMessage::E2eeDeliver {
                group,
                seq,
                sender,
                device,
                sent_at_ms,
                message,
                ..
            } => self.on_deliver(
                group, seq, &sender, device, sent_at_ms, &message, out, events,
            ),
            NetMessage::ServerError {
                request_id,
                message,
                ..
            } if is_private_request(request_id) => {
                self.starting.retain(|s| s.request != request_id);
                events.push(Event::Notice(message));
            }
            _ => {}
        }
    }

    fn on_key_packages(
        &mut self,
        request_id: u32,
        packages: Vec<cartopolis_core::E2eeKeyPackage>,
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) {
        let Some(i) = self.starting.iter().position(|s| s.request == request_id) else {
            return;
        };
        if packages.is_empty() {
            let s = self.starting.remove(i);
            events.push(Event::Notice(format!(
                "{} can't receive private messages yet: they need to sign in with a version that has them.",
                s.peer
            )));
            return;
        }
        let group = match self.device.create_group() {
            Ok(g) => g,
            Err(e) => {
                self.starting.remove(i);
                events.push(Event::Notice(format!("could not start the chat: {e}")));
                return;
            }
        };
        let request = self.request();
        let peer = self.starting[i].peer.clone();
        out.push(NetMessage::E2eeCreateGroup {
            request_id: request,
            group: group.clone(),
            direct_with: Some(peer),
        });
        self.starting[i].request = request;
        self.starting[i].stage = Stage::Creating { group, packages };
    }

    fn on_group_created(
        &mut self,
        request_id: u32,
        group: Vec<u8>,
        existing: Option<Vec<u8>>,
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) {
        let Some(i) = self.starting.iter().position(|s| s.request == request_id) else {
            return;
        };
        let peer = self.starting[i].peer.clone();
        if let Some(existing) = existing {
            // There already is a direct chat between these two people.
            self.starting.remove(i);
            self.device.forget(&group);
            if self.cursors.contains_key(&existing) {
                events.push(Event::Opened {
                    group: existing,
                    peer,
                });
            } else {
                events.push(Event::Notice(format!(
                    "You already have a private chat with {peer}, started on another device. \
                     It isn't on this one yet."
                )));
            }
            return;
        }
        let Stage::Creating {
            packages,
            group: mine,
        } = std::mem::replace(&mut self.starting[i].stage, Stage::Claiming)
        else {
            return;
        };
        if mine != group {
            // The answer is about a different chat than the one this request
            // registered. Nothing sensible to add anyone to.
            self.starting.remove(i);
            self.device.forget(&mine);
            tracing::warn!("the server confirmed a different chat than was created");
            return;
        }
        let bytes: Vec<Vec<u8>> = packages.iter().map(|p| p.bytes.clone()).collect();
        match self.device.add(&group, &bytes) {
            Ok(add) => {
                let request = self.request();
                out.push(NetMessage::E2eeSend {
                    request_id: request,
                    group: group.clone(),
                    epoch: add.epoch,
                    commit: true,
                    message: add.message,
                    welcome: add.welcome.map(|w| E2eeWelcome {
                        devices: packages.iter().map(|p| p.device).collect(),
                        members: vec![peer.clone()],
                        bytes: w,
                    }),
                });
                self.starting[i].request = request;
                self.starting[i].stage = Stage::Committing { group };
            }
            Err(e) => {
                self.starting.remove(i);
                self.device.forget(&group);
                events.push(Event::Notice(format!("could not invite {peer}: {e}")));
            }
        }
    }

    fn on_sent(
        &mut self,
        request_id: u32,
        group: &[u8],
        seq: Option<u64>,
        refused: Option<String>,
        events: &mut Vec<Event>,
    ) {
        if let Some(i) = self.starting.iter().position(|s| s.request == request_id) {
            let s = self.starting.remove(i);
            let Stage::Committing { group } = s.stage else {
                return;
            };
            match seq {
                Some(seq) => {
                    if let Err(e) = self.device.confirm_pending(&group) {
                        events.push(Event::Notice(format!("could not start the chat: {e}")));
                        return;
                    }
                    self.cursors.insert(group.clone(), seq);
                    self.peers.insert(group.clone(), s.peer.clone());
                    events.push(Event::Opened {
                        group,
                        peer: s.peer,
                    });
                }
                None => {
                    let _ = self.device.discard_pending(&group);
                    events.push(Event::Notice(refused.unwrap_or_else(|| "refused".into())));
                }
            }
            return;
        }
        if let Some(why) = refused {
            let peer = self.peers.get(group).cloned().unwrap_or_default();
            events.push(Event::Notice(format!("Not delivered to {peer}: {why}")));
        }
    }

    fn on_invite(
        &mut self,
        group: Vec<u8>,
        from: String,
        after: u64,
        welcome: &[u8],
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) {
        if self.cursors.contains_key(&group) {
            return;
        }
        match self.device.join(welcome) {
            Ok(joined) if joined == group => {
                self.cursors.insert(group.clone(), after);
                self.peers.insert(group.clone(), from.clone());
                events.push(Event::Opened {
                    group: group.clone(),
                    peer: from,
                });
                // Reporting the cursor is also what tells the server the
                // invitation is spent.
                let sync = self.sync_request(Some(&group));
                out.push(sync);
            }
            Ok(_) => tracing::warn!("an invitation opened a different chat than it named"),
            Err(e) => tracing::warn!(error = %e, "could not join a private chat"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_deliver(
        &mut self,
        group: Vec<u8>,
        seq: u64,
        sender: &str,
        device: Uuid,
        sent_at_ms: u64,
        message: &[u8],
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) {
        let Some(&cursor) = self.cursors.get(&group) else {
            return;
        };
        if seq <= cursor {
            return;
        }
        if seq > cursor + 1 {
            // Something in between never arrived (a full backlog drops lines).
            // Ask again from the cursor; the replay brings this one too.
            let sync = self.sync_request(Some(&group));
            out.push(sync);
            return;
        }
        self.cursors.insert(group.clone(), seq);
        if self.device.is_me(sender, device) {
            return;
        }
        match self.device.receive(&group, message) {
            Ok(Received::Message { from, bytes }) => {
                let body = match Payload::decode(&bytes) {
                    Ok(Payload::Text { body, .. }) => body,
                    Err(_) => "(a message this version can't display)".into(),
                };
                events.push(Event::Line {
                    group,
                    from: from.username,
                    body,
                    sent_at_ms,
                });
            }
            Ok(Received::Membership { .. }) => {}
            Err(e) => tracing::warn!(error = %e, seq, "could not read a private-chat entry"),
        }
    }
}

// ── Bevy ────────────────────────────────────────────────────────────────────

/// Private-chat messages the network system collected this frame, for
/// [`drive_private_chats`] to apply. A queue of its own rather than a second
/// drain of the mailbox, which would race the first for messages.
#[derive(Resource, Default)]
pub struct PrivateInbox(pub Vec<NetMessage>);

impl PrivateInbox {
    /// Whether a message belongs here rather than to the network system.
    #[must_use]
    pub fn wants(msg: &NetMessage) -> bool {
        match msg {
            NetMessage::E2eeDeviceStatus { .. }
            | NetMessage::E2eeKeyPackages { .. }
            | NetMessage::E2eeGroupCreated { .. }
            | NetMessage::E2eeSent { .. }
            | NetMessage::E2eeSynced { .. }
            | NetMessage::E2eeDeliver { .. }
            | NetMessage::E2eeInvite { .. } => true,
            NetMessage::ServerError { request_id, .. } => is_private_request(*request_id),
            _ => false,
        }
    }
}

/// This installation's private chats: the engine, and where it is saved.
///
/// What the UI asks for (start a chat, send a line) arrives through
/// `ChatState::private_start` / `private_outbox` rather than through here, so
/// the panels that ask need no handle on the keys.
#[derive(Resource, Default)]
pub struct PrivateChats {
    engine: Option<Engine>,
    /// Server and account the engine belongs to, i.e. where it is saved.
    key: Option<String>,
    connected: bool,
    dirty: bool,
    /// The word filter, applied here because the server cannot read a private
    /// line to mask it (`docs/notes/user-safety.md`, guideline 1.2). The
    /// built-in list only: an operator's additions live in the server's config
    /// and never reach a client.
    filter: Option<cartopolis_core::Filter>,
}

fn now_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Run the engine for this frame: connect it when a session comes up, apply
/// what arrived, send what the UI asked for, and save when anything changed.
pub fn drive_private_chats(
    online: Option<Res<crate::resources::OnlineSession>>,
    identity: Res<crate::resources::AuthIdentity>,
    addr: Option<Res<crate::resources::ConnectAddr>>,
    mut inbox: ResMut<PrivateInbox>,
    mut private: ResMut<PrivateChats>,
    mut chat: ResMut<crate::systems::chat::ChatState>,
) {
    let (Some(online), Some(username)) = (online, identity.username.clone()) else {
        private.connected = false;
        inbox.0.clear();
        return;
    };
    let server = addr.map(|a| a.0.clone()).unwrap_or_default();
    let key = state_key(&server, &username);

    // A different account or server than the engine in hand: load that one.
    if private.key.as_deref() != Some(key.as_str()) {
        let loaded = crate::platform::storage::read_now(crate::platform::storage::Namespace::Data, &key)
            .ready()
            .and_then(|b| match Engine::load(&b) {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!(error = %e, "saved private-chat keys unreadable; starting a new device");
                    None
                }
            });
        let engine = match loaded {
            Some(e) => Some(e),
            None => Engine::new(&username)
                .map_err(|e| tracing::warn!(error = %e, "no private chats"))
                .ok(),
        };
        private.engine = engine;
        private.key = Some(key.clone());
        private.connected = false;
        private.dirty = true;
    }
    let Some(mut engine) = private.engine.take() else {
        return;
    };

    let mut out = Vec::new();
    let mut events = Vec::new();
    if !private.connected {
        private.connected = true;
        engine.on_connected(&mut out);
        for (g, peer) in engine.chats() {
            chat.open_channel(&channel_key(g), &format!("private · {peer}"));
        }
        private.dirty = true;
    }
    let arrived = !inbox.0.is_empty();
    for msg in inbox.0.drain(..) {
        engine.handle(msg, &mut out, &mut events);
    }
    for name in std::mem::take(&mut chat.private_start) {
        engine.start_direct(&name, &mut out, &mut events);
    }
    for (channel, text) in std::mem::take(&mut chat.private_outbox) {
        let Some(group) = group_of(&channel) else {
            continue;
        };
        match engine.send_text(&group, &text, now_ms(), &mut out) {
            Ok(()) => {
                let me = engine.username().to_string();
                let my_id = chat.my_id.unwrap_or(0);
                chat.receive(&channel, my_id, me, text, None);
            }
            Err(e) => events.push(Event::Notice(format!("not sent: {e}"))),
        }
        private.dirty = true;
    }
    for ev in events {
        match ev {
            Event::Opened { group, peer } => {
                let key = channel_key(&group);
                chat.open_channel(&key, &format!("private · {peer}"));
                chat.focus_channel(&key);
            }
            Event::Line {
                group, from, body, ..
            } => {
                let filter = private
                    .filter
                    .get_or_insert_with(cartopolis_core::Filter::seeded);
                let (body, _) = filter.mask(&body);
                chat.receive(&channel_key(&group), 0, from, body, None);
            }
            Event::Notice(text) => {
                chat.notice = Some(crate::systems::chat::ChatNotice { text, error: true });
            }
        }
    }
    for msg in out {
        online.send("private chat", msg);
    }
    if arrived || private.dirty {
        let bytes = engine.save();
        if let Err(e) = crate::platform::storage::write_now(
            crate::platform::storage::Namespace::Data,
            &key,
            &bytes,
        ) {
            tracing::warn!(error = %e, "could not save private-chat keys");
        }
        private.dirty = false;
    }
    private.engine = Some(engine);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_key_round_trips_to_its_group() {
        let g = vec![0x00, 0xab, 0xff, 0x10];
        let key = channel_key(&g);
        assert_eq!(key, "dm:00abff10");
        assert_eq!(group_of(&key), Some(g));
        assert_eq!(group_of("room:lobby"), None);
        assert_eq!(group_of("dm:abc"), None);
    }

    #[test]
    fn private_request_ids_do_not_collide_with_the_account_deletion_id() {
        assert!(!is_private_request(
            crate::resources::DELETE_ACCOUNT_REQUEST
        ));
        assert!(!is_private_request(0));
        assert!(is_private_request(REQUEST_BASE | 7));
    }

    #[test]
    fn state_keys_cannot_climb_out_of_their_prefix() {
        let k = state_key("../../etc", "a/b");
        assert!(k.starts_with(STATE_PREFIX));
        assert!(!k[STATE_PREFIX.len()..].contains(".."), "{k}");
        assert_eq!(k.matches('/').count(), 2, "{k}");
    }
}

/// Two engines talking through a real simulator, over loopback TCP. This is the
/// test that says the client's half, the wire and the server's half fit.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod loopback {
    use super::*;
    use bytes::Bytes;
    use cartopolis_core::{decode_app_frame, encode_app_frame, AuthCredential, PROTOCOL_VERSION};
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio::net::TcpStream;
    use tokio_util::codec::{Framed, LengthDelimitedCodec};

    type Conn = Framed<TcpStream, LengthDelimitedCodec>;

    struct Peer {
        conn: Conn,
        engine: Engine,
        events: Vec<Event>,
    }

    impl Peer {
        async fn connect(addr: &str, name: &str) -> Self {
            let mut conn = Framed::new(
                TcpStream::connect(addr).await.unwrap(),
                LengthDelimitedCodec::builder()
                    .max_frame_length(32 << 20)
                    .little_endian()
                    .new_codec(),
            );
            let hello = encode_app_frame(&NetMessage::ClientHello {
                protocol_version: PROTOCOL_VERSION,
                auth: AuthCredential::Password {
                    username: name.into(),
                    password: "pw".into(),
                },
            })
            .unwrap();
            conn.send(Bytes::from(hello)).await.unwrap();
            let mut p = Self {
                conn,
                engine: Engine::new(name).unwrap(),
                events: Vec::new(),
            };
            let mut out = Vec::new();
            p.engine.on_connected(&mut out);
            p.send_all(out).await;
            p
        }

        async fn send_all(&mut self, out: Vec<NetMessage>) {
            for m in out {
                self.conn
                    .send(Bytes::from(encode_app_frame(&m).unwrap()))
                    .await
                    .unwrap();
            }
        }

        /// Feed the engine whatever arrives for `window`, answering as it goes.
        async fn pump(&mut self, window: Duration) {
            let deadline = tokio::time::Instant::now() + window;
            loop {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                let Ok(Some(Ok(frame))) = tokio::time::timeout(left, self.conn.next()).await else {
                    return;
                };
                let msg = decode_app_frame(&frame).unwrap();
                if !PrivateInbox::wants(&msg) {
                    continue;
                }
                let mut out = Vec::new();
                self.engine.handle(msg, &mut out, &mut self.events);
                self.send_all(out).await;
            }
        }

        fn lines(&self) -> Vec<(String, String)> {
            self.events
                .iter()
                .filter_map(|e| match e {
                    Event::Line { from, body, .. } => Some((from.clone(), body.clone())),
                    _ => None,
                })
                .collect()
        }

        fn opened(&self) -> Option<Vec<u8>> {
            self.events.iter().find_map(|e| match e {
                Event::Opened { group, .. } => Some(group.clone()),
                _ => None,
            })
        }
    }

    const PUMP: Duration = Duration::from_millis(600);

    async fn server(
        names: &[&str],
    ) -> (tempfile::TempDir, cartopolis_simulator::SimHandle, String) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("dm.db").to_str().unwrap().to_string();
        {
            let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
            for n in names {
                cartopolis_simulator::ensure_account(&conn, n, "pw").unwrap();
            }
        }
        let handle =
            cartopolis_simulator::serve(std::sync::Arc::new(cartopolis_simulator::SimConfig {
                database_path: db,
                listen: "127.0.0.1:0".into(),
                ..cartopolis_simulator::SimConfig::default()
            }))
            .await
            .unwrap();
        let addr = handle.local_addr.to_string();
        (dir, handle, addr)
    }

    #[tokio::test]
    async fn a_direct_chat_started_by_name_carries_lines_both_ways() {
        let (_dir, _h, addr) = server(&["alice", "bob"]).await;
        let mut alice = Peer::connect(&addr, "alice").await;
        let mut bob = Peer::connect(&addr, "bob").await;
        alice.pump(PUMP).await;
        bob.pump(PUMP).await;

        let mut out = Vec::new();
        alice
            .engine
            .start_direct("bob", &mut out, &mut alice.events);
        alice.send_all(out).await;
        alice.pump(PUMP).await;
        let group = alice.opened().expect("alice's side opened");
        bob.pump(PUMP).await;
        assert_eq!(
            bob.opened(),
            Some(group.clone()),
            "bob joined the same chat"
        );

        let mut out = Vec::new();
        alice
            .engine
            .send_text(&group, "dinner at seven?", 0, &mut out)
            .unwrap();
        alice.send_all(out).await;
        bob.pump(PUMP).await;
        assert_eq!(
            bob.lines(),
            vec![("alice".into(), "dinner at seven?".into())]
        );

        let mut out = Vec::new();
        bob.engine.send_text(&group, "yes!", 0, &mut out).unwrap();
        bob.send_all(out).await;
        alice.pump(PUMP).await;
        assert_eq!(alice.lines(), vec![("bob".into(), "yes!".into())]);
        // Neither sees their own line come back as somebody else's.
        assert!(bob.lines().iter().all(|(f, _)| f == "alice"));
    }

    #[tokio::test]
    async fn a_saved_engine_picks_up_what_arrived_while_it_was_gone() {
        let (_dir, _h, addr) = server(&["alice", "bob"]).await;
        let mut alice = Peer::connect(&addr, "alice").await;
        let mut bob = Peer::connect(&addr, "bob").await;
        alice.pump(PUMP).await;
        bob.pump(PUMP).await;
        let mut out = Vec::new();
        alice
            .engine
            .start_direct("bob", &mut out, &mut alice.events);
        alice.send_all(out).await;
        alice.pump(PUMP).await;
        bob.pump(PUMP).await;
        let group = alice.opened().unwrap();

        // Bob's app closes, saving its state.
        let saved = bob.engine.save();
        drop(bob);
        let mut out = Vec::new();
        alice
            .engine
            .send_text(&group, "while you were out", 0, &mut out)
            .unwrap();
        alice.send_all(out).await;
        alice.pump(PUMP).await;

        // Bob's app starts again from what it saved.
        let mut bob = Peer::connect(&addr, "bob").await;
        bob.engine = Engine::load(&saved).unwrap();
        let mut out = Vec::new();
        bob.engine.on_connected(&mut out);
        bob.send_all(out).await;
        bob.pump(PUMP).await;
        assert_eq!(
            bob.lines(),
            vec![("alice".into(), "while you were out".into())]
        );
    }

    #[tokio::test]
    async fn asking_twice_for_the_same_person_opens_the_same_chat() {
        let (_dir, _h, addr) = server(&["alice", "bob"]).await;
        let mut alice = Peer::connect(&addr, "alice").await;
        let mut bob = Peer::connect(&addr, "bob").await;
        alice.pump(PUMP).await;
        bob.pump(PUMP).await;
        let mut out = Vec::new();
        alice
            .engine
            .start_direct("bob", &mut out, &mut alice.events);
        alice.send_all(out).await;
        alice.pump(PUMP).await;
        bob.pump(PUMP).await;
        let first = alice.opened().unwrap();

        // Bob now asks for a chat with Alice: it must be the one they have.
        bob.events.clear();
        let mut out = Vec::new();
        bob.engine.start_direct("alice", &mut out, &mut bob.events);
        assert!(out.is_empty(), "no new chat is set up");
        assert_eq!(bob.opened(), Some(first));
    }
}
