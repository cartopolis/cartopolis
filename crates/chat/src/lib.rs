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

use cartopolis_core::address;
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

/// Longest reaction, in characters: one emoji, which may be several code
/// points (a flag, a skin tone, a family).
const MAX_EMOJI_CHARS: usize = 16;

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
        | NetMessage::E2eeInvite { .. }
        | NetMessage::Workspaces { .. }
        | NetMessage::ChannelJoinRequest { .. }
        | NetMessage::E2eeAddDevices { .. } => true,
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
        /// Its place in the chat's log: the message's id, the same on every
        /// member's device.
        seq: u64,
        from: String,
        body: String,
        point: Option<Point>,
        /// Set when what they sent was a picture; `body` is then its caption.
        picture: Option<PictureRef>,
        /// Set when the line is in the thread under the message with this id.
        parent: Option<u64>,
        sent_at_ms: u64,
    },
    /// Somebody reacted to the message at `target` (or took it back).
    Reaction {
        group: Vec<u8>,
        from: String,
        target: u64,
        emoji: String,
        remove: bool,
    },
    /// Somebody changed the text of a message. The caller applies it only if
    /// `from` sent the message at `target`: this engine keeps no history.
    Edited {
        group: Vec<u8>,
        from: String,
        target: u64,
        body: String,
    },
    /// Somebody took back a message, on the same condition as [`Event::Edited`].
    Deleted {
        group: Vec<u8>,
        from: String,
        target: u64,
    },
    /// The server accepted something this device sent: the request id the send
    /// answered with, and the place in the log — the message's id — it took.
    Sent {
        group: Vec<u8>,
        request: u32,
        seq: u64,
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
    /// Every workspace this account belongs to, whole (`NetMessage::Workspaces`):
    /// the answer to each workspace request and whenever one changes.
    Workspaces(Vec<cartopolis_core::WorkspaceDto>),
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
        // A reply is a line, with its parent beside it (`on_deliver`).
        Ok(Payload::Reply { body, .. }) => (body, None, None),
        // None of these is a line; `on_deliver` handles them before this.
        Ok(
            Payload::Location { .. }
            | Payload::LocationStopped { .. }
            | Payload::GroupName { .. }
            | Payload::React { .. }
            | Payload::Edit { .. }
            | Payload::Delete { .. },
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

/// A workspace channel being started: a chat with nobody else in it yet,
/// registered, then listed in its workspace (`NetMessage::ChannelCreate`).
#[derive(Debug)]
struct ChannelStart {
    workspace: Uuid,
    name: String,
    topic: String,
    private: bool,
    /// The `E2eeCreateGroup` in flight.
    request: u32,
    group: Vec<u8>,
}

/// Somebody being added to a chat this device is already in: by its own
/// person (a private channel, a group), or because the server handed this
/// device a join request (`NetMessage::ChannelJoinRequest`).
#[derive(Debug)]
struct Adding {
    group: Vec<u8>,
    person: String,
    /// The claim, then the commit, in flight.
    request: u32,
    committing: bool,
    /// The devices the commit invites, for its welcome.
    devices: Vec<Uuid>,
    /// Attempts left after a commit refused because somebody else changed the
    /// chat first.
    retries: u8,
}

/// How often an add is retried after losing a race with another commit.
const ADD_RETRIES: u8 = 2;

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
    channel_starts: Vec<ChannelStart>,
    adding: Vec<Adding>,
    /// Commits in flight that add more devices of people already in a chat
    /// (`NetMessage::E2eeAddDevices`): the request and the chat.
    device_adds: Vec<(u32, Vec<u8>)>,
    next_request: u32,
}

impl Engine {
    /// A brand-new device for the person at `address` (`name@server`).
    pub fn new(address: &str) -> Result<Self, String> {
        Ok(Self {
            device: Device::new(address, Uuid::new_v4()).map_err(|e| e.to_string())?,
            cursors: BTreeMap::new(),
            peers: BTreeMap::new(),
            groups: std::collections::BTreeSet::new(),
            starting: Vec::new(),
            group_starts: Vec::new(),
            channel_starts: Vec::new(),
            adding: Vec::new(),
            device_adds: Vec::new(),
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
            channel_starts: Vec::new(),
            adding: Vec::new(),
            device_adds: Vec::new(),
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

    /// This device's person, as a full address.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.device.identity().address
    }

    /// This device's home server: where its person's account is, and the
    /// home of every chat it starts.
    #[must_use]
    pub fn home(&self) -> &str {
        address::split(self.address()).map_or("", |(_, server)| server)
    }

    /// `who` as a full address, a bare name meaning somebody on this
    /// device's own server.
    #[must_use]
    pub fn qualify(&self, who: &str) -> String {
        address::qualify(who, self.home())
    }

    /// An id for a chat this device starts. It begins with this device's home
    /// server, which is the server that will order the chat's log.
    fn new_group_id(&self) -> Vec<u8> {
        address::group_id(self.home(), Uuid::new_v4().into_bytes())
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
        let me = self.address();
        let mut out: Vec<String> = Vec::new();
        for id in self.device.members(group) {
            if !id.address.eq_ignore_ascii_case(me)
                && !out.iter().any(|n| n.eq_ignore_ascii_case(&id.address))
            {
                out.push(id.address);
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

    /// The direct chat with `who` (a name on this server, or a full address),
    /// if this device is in one. Never a group, whatever the group happens to
    /// be called — the distinction [`Self::start_direct`] already makes.
    #[must_use]
    pub fn direct_with(&self, who: &str) -> Option<&Vec<u8>> {
        let who = self.qualify(who);
        self.peers
            .iter()
            .find(|(g, p)| !self.groups.contains(*g) && p.eq_ignore_ascii_case(&who))
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
        self.device_adds.clear();
    }

    /// Start a group chat called `name` with `members` (names on this server,
    /// or full addresses).
    pub fn start_group(
        &mut self,
        name: &str,
        members: &[String],
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) {
        let name = name.trim().to_string();
        let me = self.address().to_string();
        let mut people: Vec<String> = Vec::new();
        for m in members {
            let m = if m.trim().is_empty() {
                String::new()
            } else {
                self.qualify(m)
            };
            let m = m.as_str();
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
                address: person.clone(),
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
        let group = match self.device.create_group(&self.new_group_id()) {
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

    /// Start (or find) a direct chat with `who`: a name on this server, or a
    /// full address.
    pub fn start_direct(&mut self, who: &str, out: &mut Vec<NetMessage>, events: &mut Vec<Event>) {
        if who.trim().is_empty() {
            return;
        }
        let username = self.qualify(who);
        if username.eq_ignore_ascii_case(self.address()) {
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
            address: username.clone(),
        });
        self.starting.push(Starting {
            request,
            peer: username,
            stage: Stage::Claiming,
        });
    }

    // ── Workspaces and channels (protocol 38) ──────────────────────────────

    /// Create a workspace with this account as its owner. The answer arrives
    /// as [`Event::Workspaces`].
    pub fn create_workspace(&mut self, name: &str, out: &mut Vec<NetMessage>) {
        out.push(NetMessage::WorkspaceCreate {
            request_id: self.request(),
            name: name.trim().to_string(),
        });
    }

    /// Ask for every workspace this account belongs to.
    pub fn list_workspaces(&mut self, out: &mut Vec<NetMessage>) {
        out.push(NetMessage::WorkspaceList {
            request_id: self.request(),
        });
    }

    /// Add somebody to a workspace (an owner or admin's request).
    pub fn add_workspace_member(&mut self, workspace: Uuid, who: &str, out: &mut Vec<NetMessage>) {
        out.push(NetMessage::WorkspaceAddMember {
            request_id: self.request(),
            workspace,
            address: self.qualify(who),
        });
    }

    /// Start a channel in a workspace: a chat with only this account in it,
    /// listed under `name`. Others join a public one themselves
    /// ([`Self::join_channel`]) and are added to a private one
    /// ([`Self::add_to_chat`]).
    pub fn start_channel(
        &mut self,
        workspace: Uuid,
        name: &str,
        topic: &str,
        private: bool,
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) {
        // Checked here as well as on the server: a name the server refuses
        // after the chat is registered would leave an unlisted chat behind.
        if cartopolis_core::channel_name(name).is_empty() {
            events.push(Event::Notice(
                "a channel name needs at least one letter or digit".into(),
            ));
            return;
        }
        let group = match self.device.create_group(&self.new_group_id()) {
            Ok(g) => g,
            Err(e) => {
                events.push(Event::Notice(format!("could not start the channel: {e}")));
                return;
            }
        };
        let request = self.request();
        out.push(NetMessage::E2eeCreateGroup {
            request_id: request,
            group: group.clone(),
            direct_with: None,
        });
        self.channel_starts.push(ChannelStart {
            workspace,
            name: cartopolis_core::channel_name(name),
            topic: topic.trim().to_string(),
            private,
            request,
            group,
        });
    }

    /// Ask into a workspace's public channel. A member's device adds this one;
    /// the chat then arrives as an invitation, like any other.
    pub fn join_channel(&mut self, workspace: Uuid, group: &[u8], out: &mut Vec<NetMessage>) {
        out.push(NetMessage::ChannelJoin {
            request_id: self.request(),
            workspace,
            group: group.to_vec(),
        });
    }

    /// Add `who` to a group chat or channel this device is in: claim their key
    /// packages, then commit them in. Nothing happens for a direct chat (it is
    /// two people) or for somebody already in it.
    pub fn add_to_chat(
        &mut self,
        group: &[u8],
        who: &str,
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) {
        if !self.groups.contains(group) {
            events.push(Event::Notice(
                "people can only be added to a group or a channel".into(),
            ));
            return;
        }
        let person = self.qualify(who);
        let already = self
            .device
            .members(group)
            .iter()
            .any(|m| m.address.eq_ignore_ascii_case(&person));
        let underway = self
            .adding
            .iter()
            .any(|a| a.group == group && a.person.eq_ignore_ascii_case(&person));
        if already || underway {
            return;
        }
        let request = self.request();
        out.push(NetMessage::E2eeClaimKeyPackages {
            request_id: request,
            address: person.clone(),
        });
        self.adding.push(Adding {
            group: group.to_vec(),
            person,
            request,
            committing: false,
            devices: Vec::new(),
            retries: ADD_RETRIES,
        });
    }

    /// The server found devices of this chat's members that are not in it (a
    /// phone signed in after the chat was started on a browser) and handed
    /// this device their key packages: commit them in. Only devices of people
    /// already in the chat are added (`Device::add_member_devices`), so this
    /// cannot bring in anybody new. While another change to the chat is in
    /// flight it is skipped; the server asks again later.
    fn add_member_devices(
        &mut self,
        group: &[u8],
        devices: &[cartopolis_core::E2eeNewDevice],
        out: &mut Vec<NetMessage>,
    ) {
        if !self.cursors.contains_key(group) || self.changing(group) {
            return;
        }
        let bytes: Vec<Vec<u8>> = devices.iter().map(|d| d.package.bytes.clone()).collect();
        let (add, who) = match self.device.add_member_devices(group, &bytes) {
            Ok(Some(added)) => added,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "could not add a member's other devices");
                return;
            }
        };
        let mut members: Vec<String> = Vec::new();
        for w in &who {
            if !members.iter().any(|m| m.eq_ignore_ascii_case(&w.address)) {
                members.push(w.address.clone());
            }
        }
        let request = self.request();
        out.push(NetMessage::E2eeSend {
            request_id: request,
            group: group.to_vec(),
            epoch: add.epoch,
            commit: true,
            message: add.message,
            welcome: add.welcome.map(|w| E2eeWelcome {
                devices: who.iter().map(|w| w.device).collect(),
                members,
                bytes: w,
            }),
        });
        self.device_adds.push((request, group.to_vec()));
    }

    /// Whether this device has a change to `group`'s membership in flight.
    fn changing(&self, group: &[u8]) -> bool {
        self.device_adds.iter().any(|(_, g)| g == group)
            || self.adding.iter().any(|a| a.group == group)
            || self
                .starting
                .iter()
                .any(|s| matches!(&s.stage, Stage::Committing { group: g } if g == group))
            || self
                .group_starts
                .iter()
                .any(|g| g.committing && g.group.as_deref() == Some(group))
    }

    /// The server's answer to a device add's commit. Returns whether it was
    /// one. A refusal is dropped: the chat changed first, and the server asks
    /// again.
    fn on_device_add_committed(
        &mut self,
        request_id: u32,
        group: &[u8],
        seq: Option<u64>,
        out: &mut Vec<NetMessage>,
    ) -> bool {
        let Some(i) = self.device_adds.iter().position(|(r, _)| *r == request_id) else {
            return false;
        };
        self.device_adds.remove(i);
        let result = if seq.is_some() {
            self.device.confirm_pending(group)
        } else {
            self.device.discard_pending(group)
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, "could not settle a device add");
        }
        // The new device reads from after this commit: a group's name has to be
        // said again for it, as for a newcomer.
        if seq.is_some() && self.groups.contains(group) {
            if let Some(label) = self.peers.get(group).cloned() {
                let name = Payload::GroupName {
                    name: label,
                    sent_at_ms: 0,
                };
                if let Err(e) = self.send_payload(group, &name, out) {
                    tracing::warn!(error = %e, "could not repeat a chat's name for a new device");
                }
            }
        }
        true
    }

    /// The server registered a channel's chat: list it in its workspace and
    /// open it. Returns whether `request_id` was a channel start.
    fn on_channel_registered(
        &mut self,
        request_id: u32,
        group: &[u8],
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) -> bool {
        let Some(i) = self
            .channel_starts
            .iter()
            .position(|c| c.request == request_id)
        else {
            return false;
        };
        let c = self.channel_starts.remove(i);
        if c.group != group {
            self.device.forget(&c.group);
            return true;
        }
        let label = format!("#{}", c.name);
        self.cursors.insert(c.group.clone(), 0);
        self.peers.insert(c.group.clone(), label.clone());
        self.groups.insert(c.group.clone());
        out.push(NetMessage::ChannelCreate {
            request_id: self.request(),
            workspace: c.workspace,
            group: c.group.clone(),
            name: c.name,
            topic: c.topic,
            private: c.private,
        });
        events.push(Event::Opened {
            group: c.group,
            peer: label,
        });
        true
    }

    /// A claim answer for an add. Returns whether it was one.
    fn on_adding_packages(
        &mut self,
        request_id: u32,
        packages: &[cartopolis_core::E2eeKeyPackage],
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) -> bool {
        let Some(i) = self
            .adding
            .iter()
            .position(|a| a.request == request_id && !a.committing)
        else {
            return false;
        };
        if packages.is_empty() {
            let a = self.adding.remove(i);
            events.push(Event::Notice(format!(
                "{} can't be added yet: they have no device that can receive private messages.",
                a.person
            )));
            return true;
        }
        let group = self.adding[i].group.clone();
        let bytes: Vec<Vec<u8>> = packages.iter().map(|p| p.bytes.clone()).collect();
        match self.device.add(&group, &bytes) {
            Ok(add) => {
                let request = self.request();
                let a = &mut self.adding[i];
                a.devices = packages.iter().map(|p| p.device).collect();
                out.push(NetMessage::E2eeSend {
                    request_id: request,
                    group,
                    epoch: add.epoch,
                    commit: true,
                    message: add.message,
                    welcome: add.welcome.map(|w| E2eeWelcome {
                        devices: a.devices.clone(),
                        members: vec![a.person.clone()],
                        bytes: w,
                    }),
                });
                a.request = request;
                a.committing = true;
            }
            Err(e) => {
                let a = self.adding.remove(i);
                events.push(Event::Notice(format!("could not add {}: {e}", a.person)));
            }
        }
        true
    }

    /// The server's answer to an add's commit. Returns whether it was one.
    fn on_adding_committed(
        &mut self,
        request_id: u32,
        seq: Option<u64>,
        refused: Option<String>,
        out: &mut Vec<NetMessage>,
        events: &mut Vec<Event>,
    ) -> bool {
        let Some(i) = self
            .adding
            .iter()
            .position(|a| a.request == request_id && a.committing)
        else {
            return false;
        };
        let a = self.adding.remove(i);
        if seq.is_none() {
            let _ = self.device.discard_pending(&a.group);
            if a.retries > 0 {
                // Somebody else changed the chat first. Their commit is on its
                // way here; claim again and commit on top of it.
                let request = self.request();
                out.push(NetMessage::E2eeClaimKeyPackages {
                    request_id: request,
                    address: a.person.clone(),
                });
                self.adding.push(Adding {
                    request,
                    committing: false,
                    devices: Vec::new(),
                    retries: a.retries - 1,
                    ..a
                });
            } else {
                events.push(Event::Notice(format!(
                    "could not add {}: {}",
                    a.person,
                    refused.unwrap_or_else(|| "refused".into())
                )));
            }
            return true;
        }
        if let Err(e) = self.device.confirm_pending(&a.group) {
            events.push(Event::Notice(format!("could not add {}: {e}", a.person)));
            return true;
        }
        // A newcomer reads from after the commit that added them, so the name
        // the chat was given at its start never reaches them: say it again.
        if let Some(label) = self.peers.get(&a.group).cloned() {
            if let Err(e) = self.send_payload(
                &a.group,
                &Payload::GroupName {
                    name: label,
                    sent_at_ms: 0,
                },
                out,
            ) {
                tracing::warn!(error = %e, "could not repeat a chat's name for a newcomer");
            }
        }
        true
    }

    /// Seal and send a line. The caller shows it straight away: this device
    /// cannot read its own entry back when the server replays it.
    ///
    /// Every send answers the request id it went out under; when the server
    /// accepts it, [`Event::Sent`] names that id and the place in the log the
    /// message took, which is its id from then on (what a reply, a reaction,
    /// an edit or a delete names).
    pub fn send_text(
        &mut self,
        group: &[u8],
        body: &str,
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<u32, String> {
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
    ) -> Result<u32, String> {
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
    ) -> Result<u32, String> {
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

    /// Seal and send a line in the thread under the message at `parent`.
    pub fn send_reply(
        &mut self,
        group: &[u8],
        parent: u64,
        body: &str,
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<u32, String> {
        self.send_payload(
            group,
            &Payload::Reply {
                parent,
                body: body.to_string(),
                sent_at_ms: now_ms,
            },
            out,
        )
    }

    /// React to the message at `target` with `emoji`, or take it back.
    pub fn send_reaction(
        &mut self,
        group: &[u8],
        target: u64,
        emoji: &str,
        remove: bool,
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<u32, String> {
        let emoji: String = emoji.trim().chars().take(MAX_EMOJI_CHARS).collect();
        if emoji.is_empty() {
            return Err("a reaction needs an emoji".into());
        }
        self.send_payload(
            group,
            &Payload::React {
                target,
                emoji,
                remove,
                sent_at_ms: now_ms,
            },
            out,
        )
    }

    /// Replace the text of this person's own message at `target`.
    pub fn send_edit(
        &mut self,
        group: &[u8],
        target: u64,
        body: &str,
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<u32, String> {
        self.send_payload(
            group,
            &Payload::Edit {
                target,
                body: body.to_string(),
                sent_at_ms: now_ms,
            },
            out,
        )
    }

    /// Take back this person's own message at `target`.
    pub fn send_delete(
        &mut self,
        group: &[u8],
        target: u64,
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<u32, String> {
        self.send_payload(
            group,
            &Payload::Delete {
                target,
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
        .map(|_| ())
    }

    /// Tell the chat this phone stopped sharing before its time was up.
    pub fn send_location_stopped(
        &mut self,
        group: &[u8],
        now_ms: u64,
        out: &mut Vec<NetMessage>,
    ) -> Result<(), String> {
        self.send_payload(group, &Payload::LocationStopped { sent_at_ms: now_ms }, out)
            .map(|_| ())
    }

    fn send_payload(
        &mut self,
        group: &[u8],
        payload: &Payload,
        out: &mut Vec<NetMessage>,
    ) -> Result<u32, String> {
        let sealed = self
            .device
            .encrypt(group, payload)
            .map_err(|e| e.to_string())?;
        let request_id = self.request();
        out.push(NetMessage::E2eeSend {
            request_id,
            group: group.to_vec(),
            epoch: sealed.epoch,
            commit: false,
            message: sealed.message,
            welcome: None,
        });
        Ok(request_id)
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
                if self.on_adding_packages(request_id, &packages, out, events) {
                    return;
                }
                if !self.on_group_key_packages(request_id, Some(packages.clone()), out, events) {
                    self.on_key_packages(request_id, packages, out, events);
                }
            }
            NetMessage::E2eeGroupCreated {
                request_id,
                group,
                existing,
            } => {
                if self.on_channel_registered(request_id, &group, out, events) {
                    return;
                }
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
                if self.on_device_add_committed(request_id, &group, seq, out) {
                    return;
                }
                if self.on_adding_committed(request_id, seq, refused.clone(), out, events) {
                    return;
                }
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
                    if let Some(i) = self
                        .channel_starts
                        .iter()
                        .position(|c| c.request == request_id)
                    {
                        let c = self.channel_starts.remove(i);
                        self.device.forget(&c.group);
                    }
                    if let Some(i) = self.adding.iter().position(|a| a.request == request_id) {
                        let a = self.adding.remove(i);
                        if a.committing {
                            let _ = self.device.discard_pending(&a.group);
                        }
                    }
                    if let Some(i) = self.device_adds.iter().position(|(r, _)| *r == request_id) {
                        let (_, group) = self.device_adds.remove(i);
                        let _ = self.device.discard_pending(&group);
                    }
                }
                events.push(Event::Notice(message));
            }
            NetMessage::Workspaces { workspaces, .. } => {
                events.push(Event::Workspaces(workspaces));
            }
            NetMessage::E2eeAddDevices { group, devices } => {
                self.add_member_devices(&group, &devices, out);
            }
            NetMessage::ChannelJoinRequest { group, address, .. } => {
                // Only a device already in the channel can add anybody to it;
                // the server hands the request to one.
                if self.groups.contains(&group) {
                    self.add_to_chat(&group, &address, out, events);
                }
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
        let group = match self.device.create_group(&self.new_group_id()) {
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
                    "Your chat with {peer} was started on another device. It appears here \
                     as soon as that device, or one of {peer}'s, is online to add this one."
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
        } else if let Some(seq) = seq {
            events.push(Event::Sent {
                group: group.to_vec(),
                request: request_id,
                seq,
            });
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
                // Labelled by who else is in it, not by who sent the invitation:
                // that is this person themselves when their own other device
                // added this one. A group's name follows as a sealed message.
                let others = self.people_in(&group);
                let label = match others.as_slice() {
                    [only] => only.clone(),
                    _ => from,
                };
                self.peers.insert(group.clone(), label.clone());
                events.push(Event::Opened {
                    group: group.clone(),
                    peer: label,
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
            Ok(Received::Message { from, .. }) if !from.address.eq_ignore_ascii_case(sender) => {
                tracing::warn!(
                    claimed = %from.address,
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
                    from: from.address,
                    latitude,
                    longitude,
                    accuracy_m,
                    until_ms,
                }),
                Ok(Payload::LocationStopped { .. }) => events.push(Event::LocationStopped {
                    group,
                    from: from.address,
                }),
                Ok(Payload::GroupName { name, .. }) => {
                    let name: String = name.trim().chars().take(MAX_GROUP_NAME).collect();
                    if !name.is_empty() {
                        self.peers.insert(group.clone(), name.clone());
                        self.groups.insert(group.clone());
                        events.push(Event::Named { group, name });
                    }
                }
                Ok(Payload::React {
                    target,
                    emoji,
                    remove,
                    ..
                }) => events.push(Event::Reaction {
                    group,
                    from: from.address,
                    target,
                    emoji: emoji.chars().take(MAX_EMOJI_CHARS).collect(),
                    remove,
                }),
                Ok(Payload::Edit { target, body, .. }) => events.push(Event::Edited {
                    group,
                    from: from.address,
                    target,
                    body,
                }),
                Ok(Payload::Delete { target, .. }) => events.push(Event::Deleted {
                    group,
                    from: from.address,
                    target,
                }),
                decoded => {
                    let parent = match &decoded {
                        Ok(Payload::Reply { parent, .. }) => Some(*parent),
                        _ => None,
                    };
                    let (body, point, picture) = read_payload(&bytes);
                    events.push(Event::Line {
                        group,
                        seq,
                        from: from.address,
                        body,
                        point,
                        picture,
                        parent,
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
            // The server says what it is called, which is what makes `name` a
            // full address (protocol 37).
            let server_name = loop {
                let frame = conn.next().await.unwrap().unwrap();
                if let Ok(NetMessage::ServerHelloAck { server_name, .. }) = decode_app_frame(&frame)
                {
                    break server_name;
                }
            };
            let mut p = Self {
                conn,
                engine: Engine::new(&address::person(name, &server_name)).unwrap(),
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
                    // By name, as the person would see a neighbour; that the
                    // event carries the full address is pinned by the
                    // location test.
                    Event::Line { from, body, .. } => Some((
                        address::display(from, self.engine.home()).to_string(),
                        body.clone(),
                    )),
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
        assert_eq!(shared, Some(("alice@localhost".into(), 53.2194, 3_600_000)));
        assert!(bob.events.iter().any(
            |e| matches!(e, Event::LocationStopped { from, .. } if from == "alice@localhost")
        ));
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

    /// The latest workspace list a peer was told about.
    fn workspaces(p: &Peer) -> Vec<cartopolis_core::WorkspaceDto> {
        p.events
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::Workspaces(w) => Some(w.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Alice makes a workspace with Bob in it and a public channel; Bob asks
    /// into the channel, Alice's device adds him, and he can read it — named,
    /// although the name was said before he arrived.
    async fn a_channel_with_bob_asking_in(
        alice_online_when_he_asks: bool,
    ) -> (Peer, Peer, Vec<u8>, tempfile::TempDir, String) {
        let (dir, _h, addr) = server(&["alice", "bob"]).await;
        std::mem::forget(_h);
        let mut alice = Peer::connect(&addr, "alice").await;
        let mut bob = Peer::connect(&addr, "bob").await;
        alice.pump(PUMP).await;
        bob.pump(PUMP).await;

        let mut out = Vec::new();
        alice.engine.create_workspace("Atelier Noord", &mut out);
        alice.send_all(out).await;
        alice.pump(PUMP).await;
        let ws = workspaces(&alice)[0].id;
        let mut out = Vec::new();
        alice.engine.add_workspace_member(ws, "bob", &mut out);
        alice.engine.start_channel(
            ws,
            "#General",
            "everything",
            false,
            &mut out,
            &mut alice.events,
        );
        alice.send_all(out).await;
        alice.pump(PUMP).await;
        let channel = workspaces(&alice)[0].channels[0].clone();
        assert_eq!(channel.name, "general");
        assert!(channel.joined);

        bob.pump(PUMP).await;
        let seen = workspaces(&bob);
        assert_eq!(
            seen[0].channels[0].group, channel.group,
            "bob sees the channel"
        );
        assert!(!seen[0].channels[0].joined);

        let saved = alice.engine.save();
        // Alice's app closes before Bob asks, in the second case.
        let still_here = if alice_online_when_he_asks {
            Some(alice)
        } else {
            drop(alice);
            None
        };
        let mut out = Vec::new();
        bob.engine.join_channel(ws, &channel.group, &mut out);
        bob.send_all(out).await;
        bob.pump(PUMP).await;
        assert!(
            workspaces(&bob)[0].channels[0].pending,
            "waiting for a member"
        );
        let mut alice = match still_here {
            Some(alice) => alice,
            None => {
                let mut alice = Peer::connect(&addr, "alice").await;
                alice.engine = Engine::load(&saved).unwrap();
                let mut out = Vec::new();
                alice.engine.on_connected(&mut out);
                alice.send_all(out).await;
                alice
            }
        };
        // Alice's device is handed the request, adds Bob, and he joins.
        alice.pump(PUMP).await;
        bob.pump(PUMP).await;
        alice.pump(PUMP).await;
        bob.pump(PUMP).await;
        (alice, bob, channel.group, dir, addr)
    }

    #[tokio::test]
    async fn a_member_asks_into_a_public_channel_and_a_member_device_lets_him_in() {
        let (mut alice, mut bob, group, _dir, _addr) = a_channel_with_bob_asking_in(true).await;
        assert!(
            bob.events
                .iter()
                .any(|e| matches!(e, Event::Named { group: g, name } if *g == group && name == "#general")),
            "the newcomer learns the channel's name: {:?}",
            bob.events
        );
        let mut out = Vec::new();
        alice
            .engine
            .send_text(&group, "welcome bob", 0, &mut out)
            .unwrap();
        alice.send_all(out).await;
        alice.pump(PUMP).await;
        bob.pump(PUMP).await;
        assert_eq!(bob.lines(), vec![("alice".into(), "welcome bob".into())]);
        let seen = workspaces(&bob);
        assert!(seen[0].channels[0].joined && !seen[0].channels[0].pending);
    }

    #[tokio::test]
    async fn a_request_made_while_every_member_is_away_is_settled_when_one_returns() {
        let (_alice, bob, group, _dir, _addr) = a_channel_with_bob_asking_in(false).await;
        assert!(
            bob.events
                .iter()
                .any(|e| matches!(e, Event::Opened { group: g, .. } if *g == group)),
            "bob is in once alice's device comes back: {:?}",
            bob.events
        );
    }

    /// A message's id is its place in the log, the same on both sides, and
    /// replies, reactions, edits and deletes all name it.
    #[tokio::test]
    async fn threads_reactions_edits_and_deletes_name_a_message_by_its_place_in_the_log() {
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
        let request = alice
            .engine
            .send_text(&group, "lunch?", 0, &mut out)
            .unwrap();
        alice.send_all(out).await;
        alice.pump(PUMP).await;
        let seq = alice
            .events
            .iter()
            .find_map(|e| match e {
                Event::Sent {
                    request: r, seq, ..
                } if *r == request => Some(*seq),
                _ => None,
            })
            .expect("the server placed alice's line");
        bob.pump(PUMP).await;
        assert!(bob
            .events
            .iter()
            .any(|e| matches!(e, Event::Line { seq: s, parent: None, .. } if *s == seq)));

        // Bob answers in a thread under it; Alice reacts, edits, then deletes.
        let mut out = Vec::new();
        bob.engine
            .send_reply(&group, seq, "the usual place", 0, &mut out)
            .unwrap();
        bob.send_all(out).await;
        let mut out = Vec::new();
        alice
            .engine
            .send_reaction(&group, seq, "👍", false, 0, &mut out)
            .unwrap();
        alice
            .engine
            .send_edit(&group, seq, "lunch at one?", 0, &mut out)
            .unwrap();
        alice.engine.send_delete(&group, seq, 0, &mut out).unwrap();
        alice.send_all(out).await;
        alice.pump(PUMP).await;
        bob.pump(PUMP).await;

        assert!(alice.events.iter().any(|e| matches!(
            e,
            Event::Line { parent: Some(p), body, .. } if *p == seq && body == "the usual place"
        )));
        assert!(bob.events.iter().any(|e| matches!(
            e,
            Event::Reaction { target, emoji, remove: false, .. } if *target == seq && emoji == "👍"
        )));
        assert!(bob.events.iter().any(|e| matches!(
            e,
            Event::Edited { target, body, .. } if *target == seq && body == "lunch at one?"
        )));
        assert!(bob
            .events
            .iter()
            .any(|e| matches!(e, Event::Deleted { target, .. } if *target == seq)));
        // None of the three is a line of its own.
        assert_eq!(bob.lines(), vec![("alice".into(), "lunch?".into())]);
    }
}
