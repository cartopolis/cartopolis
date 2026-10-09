//! Private chats as a state machine: one device's encrypted conversations,
//! driven by protocol messages (`docs/notes/encrypted-chat.md`, protocol 25).
//!
//! [`Engine`] is plain logic — protocol messages in, protocol messages and
//! [`Event`]s out, no I/O — so every client drives the same one: the Bevy app
//! (`cartopolis::systems::private_chat`, which saves it and draws its events as
//! chat lines) and the web UI (`cartopolis_web_chat`, the same engine compiled
//! for the browser and run in a shared worker). The suite at the bottom runs two
//! of them against a **real simulator** over loopback and reads back what one
//! typed on the other.
//!
//! A crate of its own so the web UI does not have to link the client to reach
//! it. **Nothing here may depend on `bevy`.** The keys themselves are
//! `cartopolis_e2ee`'s; this is what to send and when.

use std::collections::BTreeMap;

use cartopolis_core::point::Point;
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

/// Longest group name kept, in characters.
const MAX_GROUP_NAME: usize = 60;

/// Request ids for private-chat traffic live in their own range, so a refusal
/// (`ServerError`) can be told apart from one about a prim edit — the network
/// system deselects the prim on a 403, which is the wrong answer to "that
/// device is registered to another account".
pub const REQUEST_BASE: u32 = 0xE2EE_0000;

/// Whether a request id belongs to private chats.
#[must_use]
pub fn is_private_request(request_id: u32) -> bool {
    request_id & 0xFFFF_0000 == REQUEST_BASE
}

/// Whether a message is private-chat traffic, i.e. something to hand to
/// [`Engine::handle`] rather than to whatever else the client does with the wire.
#[must_use]
pub fn is_private_message(msg: &NetMessage) -> bool {
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

/// Where a picture lives and what opens it: the half of `Payload::Picture` a
/// line keeps.
#[derive(Clone, Debug, PartialEq)]
pub struct PictureRef {
    pub hash: String,
    pub key: [u8; 32],
    pub nonce: [u8; 12],
    pub width: u32,
    pub height: u32,
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
    /// Somebody said something in one. `point` is set when what they sent was a
    /// place; `body` is then its caption, or its name.
    Line {
        group: Vec<u8>,
        from: String,
        body: String,
        point: Option<Point>,
        /// Set when what they sent was a picture; `body` is then its caption.
        picture: Option<PictureRef>,
        sent_at_ms: u64,
    },
    /// Somebody in a chat shared where their phone is, until `until_ms`.
    Location {
        group: Vec<u8>,
        from: String,
        latitude: f64,
        longitude: f64,
        accuracy_m: f32,
        until_ms: u64,
    },
    /// They stopped sharing it early.
    LocationStopped { group: Vec<u8>, from: String },
    /// A group chat has (or changed) its name.
    Named { group: Vec<u8>, name: String },
    /// Something the person should be told, in words.
    Notice(String),
}

/// What a sealed payload says, as a line of text and the point it carries.
///
/// Anything this build cannot read becomes a line saying so rather than an
/// error: the entry has been applied either way (the ratchet moved on), and a
/// chat that silently skips a message reads as a friend who never answered.
type ReadLine = (String, Option<Point>, Option<PictureRef>);

fn read_payload(bytes: &[u8]) -> ReadLine {
    const UNREADABLE: &str = "(a message this version can't display)";
    match Payload::decode(bytes) {
        Ok(Payload::Text { body, .. }) => (body, None, None),
        Ok(Payload::Object { json, .. }) => match Point::from_activity_json(&json) {
            Ok(point) => (point_caption(&point), Some(point), None),
            Err(_) => (UNREADABLE.into(), None, None),
        },
        Ok(Payload::Picture {
            hash,
            key,
            nonce,
            width,
            height,
            caption,
            ..
        }) => (
            picture_caption(&caption),
            None,
            Some(PictureRef {
                hash,
                key,
                nonce,
                width,
                height,
            }),
        ),
        // Location updates are not lines; `on_deliver` handles them before this.
        Ok(
            Payload::Location { .. } | Payload::LocationStopped { .. } | Payload::GroupName { .. },
        )
        | Err(_) => (UNREADABLE.into(), None, None),
    }
}

/// How a picture reads as a line of text — in the chat list, a notification,
/// an agent's transcript: that it is a picture, and its caption.
///
/// Words, not 📷: that is not in the bundled font and paints as a tofu box.
/// The thread draws `icons::CAMERA` beside the picture instead.
#[must_use]
pub fn picture_caption(caption: &str) -> String {
    let caption = caption.trim();
    if caption.is_empty() {
        PICTURE_LINE.to_string()
    } else {
        format!("{PICTURE_LINE}: {caption}")
    }
}

/// The text of a picture line with no caption, and the prefix of one with.
pub const PICTURE_LINE: &str = "Picture";

/// The caption back out of a picture line's text, if it has one.
#[must_use]
pub fn caption_of(line_body: &str) -> Option<&str> {
    line_body
        .strip_prefix(PICTURE_LINE)
        .and_then(|rest| rest.strip_prefix(": "))
        .filter(|c| !c.is_empty())
}

/// How a point reads as a line of chat: its text, else its name, else "a place".
#[must_use]
pub fn point_caption(point: &Point) -> String {
    point
        .text
        .clone()
        .or_else(|| point.name.clone())
        .unwrap_or_else(|| "a place".into())
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

/// A group chat being set up. Every member's key packages are asked for at
/// once; the chat is created when all the answers are in, and everybody who
/// had a device is added in one commit.
#[derive(Debug)]
struct GroupStart {
    name: String,
    /// Claims not answered yet: (request id, member).
    waiting: Vec<(u32, String)>,
    /// Members whose devices can be added, with their packages.
    got: Vec<(String, Vec<cartopolis_core::E2eeKeyPackage>)>,
    /// Members who could not be added (no device, or refused).
    missing: Vec<String>,
    /// The request now in flight once claiming is done: registering the chat,
    /// then committing its members.
    request: Option<u32>,
    group: Option<Vec<u8>>,
    committing: bool,
}

#[derive(Serialize, Deserialize)]
struct SavedEngine {
    device: Vec<u8>,
    cursors: BTreeMap<Vec<u8>, u64>,
    peers: BTreeMap<Vec<u8>, String>,
    groups: std::collections::BTreeSet<Vec<u8>>,
}

/// One device's private chats, as a state machine over protocol messages.
pub struct Engine {
    device: Device,
    /// Per chat: the last log entry this device has applied. Entries are applied
    /// strictly in order; a gap means something was missed, and the answer is to
    /// ask again rather than to skip ahead (a skipped commit breaks the chat).
    cursors: BTreeMap<Vec<u8>, u64>,
    /// Per chat: its label — who a direct chat is with, or a group's name.
    peers: BTreeMap<Vec<u8>, String>,
    /// Which chats are groups, so a direct chat with "bob" is never mistaken for
    /// a group somebody happened to name "bob".
    groups: std::collections::BTreeSet<Vec<u8>>,
    starting: Vec<Starting>,
    group_starts: Vec<GroupStart>,
    next_request: u32,
}

impl Engine {
    /// A brand-new device for `username`.
    pub fn new(username: &str) -> Result<Self, String> {
        Ok(Self {
            device: Device::new(username, Uuid::new_v4()).map_err(|e| e.to_string())?,
            cursors: BTreeMap::new(),
            peers: BTreeMap::new(),
            groups: std::collections::BTreeSet::new(),
            starting: Vec::new(),
            group_starts: Vec::new(),
            next_request: 1,
        })
    }

    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        let saved: SavedEngine = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
        Ok(Self {
            device: Device::load(&saved.device).map_err(|e| e.to_string())?,
            cursors: saved.cursors,
            peers: saved.peers,
            groups: saved.groups,
            starting: Vec::new(),
            group_starts: Vec::new(),
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
            groups: self.groups.clone(),
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

    /// Everybody in a chat but this account, one entry per person however many
    /// devices they have. A direct chat whose other side has not joined yet
    /// still names them, from its label.
    #[must_use]
    pub fn people_in(&self, group: &[u8]) -> Vec<String> {
        let me = self.username();
        let mut out: Vec<String> = Vec::new();
        for id in self.device.members(group) {
            if !id.username.eq_ignore_ascii_case(me)
                && !out.iter().any(|n| n.eq_ignore_ascii_case(&id.username))
            {
                out.push(id.username);
            }
        }
        if out.is_empty() && !self.groups.contains(group) {
            out.extend(self.peers.get(group).cloned());
        }
        out
    }

    /// Every direct chat this device is in, with who it is with.
    pub fn directs(&self) -> impl Iterator<Item = (&Vec<u8>, &String)> {
        self.peers.iter().filter(|(g, _)| !self.groups.contains(*g))
    }

    /// Drop a chat from this device: its keys, its cursor and its label. What
    /// declining a request does locally — the server has already stopped
    /// delivering it — and nothing is sent, so the other side sees no change.
    pub fn forget(&mut self, group: &[u8]) {
        self.cursors.remove(group);
        self.peers.remove(group);
        self.groups.remove(group);
        self.device.forget(group);
    }

    /// The direct chat with `username`, if this device is in one. Never a group,
    /// whatever the group happens to be called — the distinction
    /// [`Self::start_direct`] already makes.
    #[must_use]
    pub fn direct_with(&self, username: &str) -> Option<&Vec<u8>> {
        self.peers
            .iter()
            .find(|(g, p)| !self.groups.contains(*g) && p.eq_ignore_ascii_case(username))
            .map(|(g, _)| g)
    }

    /// What a call in this chat needs from the chat's current epoch: its call
    /// key and who sends under which index (`docs/notes/calls.md`). `key_len` is
    /// the caller's cipher's key size.
    pub fn call_epoch(
        &self,
        group: &[u8],
        key_len: usize,
    ) -> Result<cartopolis_e2ee::CallEpoch, cartopolis_e2ee::Error> {
        self.device.call_epoch(group, key_len)
    }

    /// Whether a chat is a group, and its label: the other person for a direct
    /// chat, the name its creator gave it for a group.
    #[must_use]
    pub fn describe(&self, group: &[u8]) -> Option<(bool, &str)> {
        self.peers
            .get(group)
            .map(|p| (self.groups.contains(group), p.as_str()))
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
        self.group_starts.clear();
    }

    /// Start a group chat called `name` with `members` (usernames).
    pub fn start_group(
        &mut self,
        name: &str,
        members: &[String],
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) {
        let name = name.trim().to_string();
        let me = self.username().to_string();
        let mut people: Vec<String> = Vec::new();
        for m in members {
            let m = m.trim();
            if !m.is_empty()
                && !m.eq_ignore_ascii_case(&me)
                && !people.iter().any(|p| p.eq_ignore_ascii_case(m))
            {
                people.push(m.to_string());
            }
        }
        if name.is_empty() || people.is_empty() {
            events.push(Event::Notice(
                "A group needs a name and at least one other person.".into(),
            ));
            return;
        }
        let mut waiting = Vec::new();
        for person in people {
            let request = self.request();
            out.push(NetMessage::E2eeClaimKeyPackages {
                request_id: request,
                username: person.clone(),
            });
            waiting.push((request, person));
        }
        self.group_starts.push(GroupStart {
            name,
            waiting,
            got: Vec::new(),
            missing: Vec::new(),
            request: None,
            group: None,
            committing: false,
        });
    }

    /// Every claim of a group start is answered: create the chat, or give up
    /// if nobody could be added.
    fn proceed_group(&mut self, i: usize, out: &mut Vec<NetMessage>, events: &mut Vec<Event>) {
        if self.group_starts[i].got.is_empty() {
            let gs = self.group_starts.remove(i);
            events.push(Event::Notice(format!(
                "Nobody in \"{}\" can receive private messages yet: {}.",
                gs.name,
                gs.missing.join(", ")
            )));
            return;
        }
        let group = match self.device.create_group() {
            Ok(g) => g,
            Err(e) => {
                self.group_starts.remove(i);
                events.push(Event::Notice(format!("could not start the group: {e}")));
                return;
            }
        };
        let request = self.request();
        out.push(NetMessage::E2eeCreateGroup {
            request_id: request,
            group: group.clone(),
            direct_with: None,
        });
        self.group_starts[i].request = Some(request);
        self.group_starts[i].group = Some(group);
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
        if let Some(g) = self.direct_with(&username) {
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
        self.send_payload(
            group,
            &Payload::Text {
                body: body.to_string(),
                sent_at_ms: now_ms,
            },
            out,
        )
    }

    /// Seal and send a point: a place and the view it was seen from, as an
    /// ActivityStreams object (`cartopolis_core::point`). `link` is the web
    /// address of the same view, for any reader that ignores the extension.
    pub fn send_point(
        &mut self,
        group: &[u8],
        point: &Point,
        link: Option<String>,
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<(), String> {
        let json = point.to_activity_json(link).map_err(|e| e.to_string())?;
        self.send_payload(
            group,
            &Payload::Object {
                json,
                sent_at_ms: now_ms,
            },
            out,
        )
    }

    /// Seal and send a picture that is already in the picture store: its hash
    /// and the key that opens it (`chat_pictures`).
    pub fn send_picture(
        &mut self,
        group: &[u8],
        picture: &PictureRef,
        caption: &str,
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<(), String> {
        self.send_payload(
            group,
            &Payload::Picture {
                hash: picture.hash.clone(),
                key: picture.key,
                nonce: picture.nonce,
                width: picture.width,
                height: picture.height,
                caption: caption.to_string(),
                sent_at_ms: now_ms,
            },
            out,
        )
    }

    /// Seal and send where this phone is, shared with the chat until `until_ms`.
    pub fn send_location(
        &mut self,
        group: &[u8],
        fix: (f64, f64, f32),
        until_ms: u64,
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<(), String> {
        let (latitude, longitude, accuracy_m) = fix;
        self.send_payload(
            group,
            &Payload::Location {
                latitude,
                longitude,
                accuracy_m,
                until_ms,
                sent_at_ms: now_ms,
            },
            out,
        )
    }

    /// Tell the chat this phone stopped sharing before its time was up.
    pub fn send_location_stopped(
        &mut self,
        group: &[u8],
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<(), String> {
        self.send_payload(group, &Payload::LocationStopped { sent_at_ms: now_ms }, out)
    }

    fn send_payload(
        &mut self,
        group: &[u8],
        payload: &Payload,
        out: &mut Vec<NetMessage>,
    ) -> Result<(), String> {
        let sealed = self
            .device
            .encrypt(group, payload)
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
            } => {
                if !self.on_group_key_packages(request_id, Some(packages.clone()), out, events) {
                    self.on_key_packages(request_id, packages, out, events);
                }
            }
            NetMessage::E2eeGroupCreated {
                request_id,
                group,
                existing,
            } => {
                if !self.on_group_registered(request_id, &group, out, events) {
                    self.on_group_created(request_id, group, existing, out, events);
                }
            }
            NetMessage::E2eeSent {
                request_id,
                group,
                seq,
                refused,
            } => {
                if !self.on_group_committed(request_id, seq, refused.clone(), out, events) {
                    self.on_sent(request_id, &group, seq, refused, events);
                }
            }
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
                // A refused claim in a group start leaves that member out and
                // lets the rest go ahead; anything else ends what it belonged to.
                if !self.on_group_key_packages(request_id, None, out, events) {
                    self.starting.retain(|s| s.request != request_id);
                    self.group_starts.retain(|g| g.request != Some(request_id));
                }
                events.push(Event::Notice(message));
            }
            _ => {}
        }
    }

    /// A claim answer, if it belongs to a group start. `None` means the claim
    /// was refused. Returns whether it did belong to one.
    fn on_group_key_packages(
        &mut self,
        request_id: u32,
        packages: Option<Vec<cartopolis_core::E2eeKeyPackage>>,
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) -> bool {
        let Some(i) = self
            .group_starts
            .iter()
            .position(|g| g.waiting.iter().any(|(r, _)| *r == request_id))
        else {
            return false;
        };
        let gs = &mut self.group_starts[i];
        let at = gs
            .waiting
            .iter()
            .position(|(r, _)| *r == request_id)
            .unwrap_or(0);
        let (_, member) = gs.waiting.remove(at);
        match packages {
            Some(p) if !p.is_empty() => gs.got.push((member, p)),
            _ => gs.missing.push(member),
        }
        if gs.waiting.is_empty() {
            self.proceed_group(i, out, events);
        }
        true
    }

    /// The server registered a group's id: add everybody in one commit.
    fn on_group_registered(
        &mut self,
        request_id: u32,
        group: &[u8],
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) -> bool {
        let Some(i) = self
            .group_starts
            .iter()
            .position(|g| g.request == Some(request_id) && !g.committing)
        else {
            return false;
        };
        if self.group_starts[i].group.as_deref() != Some(group) {
            let gs = self.group_starts.remove(i);
            if let Some(g) = gs.group {
                self.device.forget(&g);
            }
            tracing::warn!("the server confirmed a different group than was created");
            return true;
        }
        let packages: Vec<cartopolis_core::E2eeKeyPackage> = self.group_starts[i]
            .got
            .iter()
            .flat_map(|(_, p)| p.iter().cloned())
            .collect();
        let members: Vec<String> = self.group_starts[i]
            .got
            .iter()
            .map(|(m, _)| m.clone())
            .collect();
        let bytes: Vec<Vec<u8>> = packages.iter().map(|p| p.bytes.clone()).collect();
        match self.device.add(group, &bytes) {
            Ok(add) => {
                let request = self.request();
                out.push(NetMessage::E2eeSend {
                    request_id: request,
                    group: group.to_vec(),
                    epoch: add.epoch,
                    commit: true,
                    message: add.message,
                    welcome: add.welcome.map(|w| E2eeWelcome {
                        devices: packages.iter().map(|p| p.device).collect(),
                        members,
                        bytes: w,
                    }),
                });
                self.group_starts[i].request = Some(request);
                self.group_starts[i].committing = true;
            }
            Err(e) => {
                let gs = self.group_starts.remove(i);
                self.device.forget(group);
                events.push(Event::Notice(format!(
                    "could not add people to \"{}\": {e}",
                    gs.name
                )));
            }
        }
        true
    }

    /// The server accepted (or refused) the commit that adds a group's members.
    fn on_group_committed(
        &mut self,
        request_id: u32,
        seq: Option<u64>,
        refused: Option<String>,
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) -> bool {
        let Some(i) = self
            .group_starts
            .iter()
            .position(|g| g.request == Some(request_id) && g.committing)
        else {
            return false;
        };
        let gs = self.group_starts.remove(i);
        let Some(group) = gs.group else {
            return true;
        };
        let Some(seq) = seq else {
            let _ = self.device.discard_pending(&group);
            self.device.forget(&group);
            events.push(Event::Notice(refused.unwrap_or_else(|| "refused".into())));
            return true;
        };
        if let Err(e) = self.device.confirm_pending(&group) {
            events.push(Event::Notice(format!("could not start the group: {e}")));
            return true;
        }
        self.cursors.insert(group.clone(), seq);
        self.peers.insert(group.clone(), gs.name.clone());
        self.groups.insert(group.clone());
        // The name goes in sealed, as the chat's first message.
        if let Err(e) = self.send_payload(
            &group,
            &Payload::GroupName {
                name: gs.name.clone(),
                sent_at_ms: 0,
            },
            out,
        ) {
            tracing::warn!(error = %e, "could not send a group's name");
        }
        if !gs.missing.is_empty() {
            events.push(Event::Notice(format!(
                "Not added to \"{}\" (they can't receive private messages yet): {}.",
                gs.name,
                gs.missing.join(", ")
            )));
        }
        events.push(Event::Opened {
            group,
            peer: gs.name,
        });
        true
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
            // **The name inside the entry is the sender's own claim.** An MLS
            // credential is written by the device that made it, and the server
            // registers devices without looking inside their key packages — so
            // an account called `mallory` can mint a credential that says
            // `jeroen`, and every line it sends would be shown under that name.
            // `sender` is the account the server took the entry from, which is
            // the one name here nobody can choose. The two must agree, and an
            // entry where they do not is dropped rather than shown with a
            // warning: the ratchet has already moved past it, and a line from
            // somebody pretending to be somebody else has nothing in it worth
            // reading.
            Ok(Received::Message { from, .. }) if !from.username.eq_ignore_ascii_case(sender) => {
                tracing::warn!(
                    claimed = %from.username,
                    account = %sender,
                    seq,
                    "dropped a private-chat entry whose sender claims another account's name"
                );
            }
            Ok(Received::Message { from, bytes }) => match Payload::decode(&bytes) {
                Ok(Payload::Location {
                    latitude,
                    longitude,
                    accuracy_m,
                    until_ms,
                    ..
                }) => events.push(Event::Location {
                    group,
                    from: from.username,
                    latitude,
                    longitude,
                    accuracy_m,
                    until_ms,
                }),
                Ok(Payload::LocationStopped { .. }) => events.push(Event::LocationStopped {
                    group,
                    from: from.username,
                }),
                Ok(Payload::GroupName { name, .. }) => {
                    let name: String = name.trim().chars().take(MAX_GROUP_NAME).collect();
                    if !name.is_empty() {
                        self.peers.insert(group.clone(), name.clone());
                        self.groups.insert(group.clone());
                        events.push(Event::Named { group, name });
                    }
                }
                _ => {
                    let (body, point, picture) = read_payload(&bytes);
                    events.push(Event::Line {
                        group,
                        from: from.username,
                        body,
                        point,
                        picture,
                        sent_at_ms,
                    });
                }
            },
            Ok(Received::Membership { .. }) => {}
            Err(e) => tracing::warn!(error = %e, seq, "could not read a private-chat entry"),
        }
    }
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
    fn private_request_ids_are_their_own_range() {
        assert!(!is_private_request(0));
        assert!(is_private_request(REQUEST_BASE | 7));
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
                if !is_private_message(&msg) {
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
            // Everyone a contact of everyone: a chat with a stranger starts as a
            // request (protocol 27), which is `systems::contacts`' concern.
            conn.execute(
                "INSERT INTO contacts (account, contact, since_ms)
                 SELECT a.id, b.id, 0 FROM accounts a, accounts b WHERE a.id <> b.id",
                [],
            )
            .unwrap();
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
    async fn a_point_arrives_as_the_same_place_and_view() {
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

        let sent = Point {
            latitude: 53.2166,
            longitude: 6.5708,
            name: Some("Stadsschouwburg".into()),
            text: Some("then the show after?".into()),
            view: Some(cartopolis_core::point::PointView {
                altitude: 120.0,
                pitch: -35.0,
                yaw: 40.0,
            }),
            meetup: None,
        };
        let mut out = Vec::new();
        alice
            .engine
            .send_point(&group, &sent, None, 0, &mut out)
            .unwrap();
        alice.send_all(out).await;
        bob.pump(PUMP).await;
        let got = bob.events.iter().find_map(|e| match e {
            Event::Line { point, body, .. } => point.clone().map(|p| (p, body.clone())),
            _ => None,
        });
        assert_eq!(got, Some((sent, "then the show after?".into())));
    }

    #[tokio::test]
    async fn a_group_reaches_everyone_who_can_join_and_names_whoever_cannot() {
        // Dave has an account but has never signed in with private chats.
        let (_dir, _h, addr) = server(&["alice", "bob", "carol", "dave"]).await;
        let mut alice = Peer::connect(&addr, "alice").await;
        let mut bob = Peer::connect(&addr, "bob").await;
        let mut carol = Peer::connect(&addr, "carol").await;
        for p in [&mut alice, &mut bob, &mut carol] {
            p.pump(PUMP).await;
        }
        let mut out = Vec::new();
        let members = ["bob".to_string(), "carol".to_string(), "dave".to_string()];
        alice
            .engine
            .start_group("Saturday ride", &members, &mut out, &mut alice.events);
        alice.send_all(out).await;
        alice.pump(PUMP).await;
        let group = alice.opened().expect("the group opened for alice");
        assert!(
            alice
                .events
                .iter()
                .any(|e| matches!(e, Event::Notice(n) if n.contains("dave"))),
            "dave is named as not added: {:?}",
            alice.events
        );

        for p in [&mut bob, &mut carol] {
            p.pump(PUMP).await;
            assert_eq!(p.opened(), Some(group.clone()));
            assert!(
                p.events
                    .iter()
                    .any(|e| matches!(e, Event::Named { name, .. } if name == "Saturday ride")),
                "the group's name arrives sealed"
            );
        }

        let mut out = Vec::new();
        alice
            .engine
            .send_text(&group, "10:00 at the station", 0, &mut out)
            .unwrap();
        alice.send_all(out).await;
        for p in [&mut bob, &mut carol] {
            p.pump(PUMP).await;
            assert_eq!(
                p.lines(),
                vec![("alice".to_string(), "10:00 at the station".to_string())]
            );
        }
        // A direct chat with bob is still its own chat, not this group.
        bob.events.clear();
        let mut out = Vec::new();
        bob.engine.start_direct("alice", &mut out, &mut bob.events);
        assert!(!out.is_empty(), "a new direct chat is set up");
    }

    #[tokio::test]
    async fn a_shared_location_arrives_with_its_end_and_so_does_stopping() {
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

        let mut out = Vec::new();
        alice
            .engine
            .send_location(&group, (53.2194, 6.5665, 8.0), 3_600_000, 0, &mut out)
            .unwrap();
        alice
            .engine
            .send_location_stopped(&group, 1, &mut out)
            .unwrap();
        alice.send_all(out).await;
        bob.pump(PUMP).await;
        let shared = bob.events.iter().find_map(|e| match e {
            Event::Location {
                from,
                latitude,
                until_ms,
                ..
            } => Some((from.clone(), *latitude, *until_ms)),
            _ => None,
        });
        assert_eq!(shared, Some(("alice".into(), 53.2194, 3_600_000)));
        assert!(bob
            .events
            .iter()
            .any(|e| matches!(e, Event::LocationStopped { from, .. } if from == "alice")));
        // And neither shows up as a chat line.
        assert!(bob.lines().is_empty());
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
