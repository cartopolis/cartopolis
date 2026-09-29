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
//!   registered; earlier ones would need the encrypted backup (recovery key),
//!   which is set aside for now.
//! - **Changing a group after it starts.** A group is created with its members
//!   and its name; adding or removing people later is not built.

use std::collections::BTreeMap;

use bevy::prelude::*;
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

/// How a private chat is labelled in lists: a padlock, then who it is with.
/// The padlock is the whole of how the UI says "encrypted" at a glance, so it
/// is on every private chat and on nothing else.
#[must_use]
pub fn private_label(peer: &str) -> String {
    format!("{} {peer}", crate::systems::icons::LOCK)
}

/// Who a private chat is with, from its label ([`private_label`]).
#[must_use]
pub fn peer_of_label(label: &str) -> &str {
    label
        .strip_prefix(crate::systems::icons::LOCK)
        .map_or(label, str::trim_start)
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
fn read_payload(bytes: &[u8]) -> (String, Option<Point>) {
    const UNREADABLE: &str = "(a message this version can't display)";
    match Payload::decode(bytes) {
        Ok(Payload::Text { body, .. }) => (body, None),
        Ok(Payload::Object { json, .. }) => match Point::from_activity_json(&json) {
            Ok(point) => (point_caption(&point), Some(point)),
            Err(_) => (UNREADABLE.into(), None),
        },
        // Location updates are not lines; `on_deliver` handles them before this.
        Ok(
            Payload::Location { .. } | Payload::LocationStopped { .. } | Payload::GroupName { .. },
        )
        | Err(_) => (UNREADABLE.into(), None),
    }
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

    /// Whether a chat is a group, and its label: the other person for a direct
    /// chat, the name its creator gave it for a group.
    #[cfg(mcp)]
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
                    let (body, point) = read_payload(&bytes);
                    events.push(Event::Line {
                        group,
                        from: from.username,
                        body,
                        point,
                        sent_at_ms,
                    });
                }
            },
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
    /// The phone's location watch, held while any share is live and dropped
    /// the moment none is, so the radio is not kept on for nothing.
    watch: Option<crate::platform::location::LocationWatch>,
    /// Per chat being shared with: when and where the last update was sent.
    last_sent: std::collections::HashMap<String, (u64, f64, f64)>,
}

impl PrivateChats {
    /// What a call in this chat needs from its current epoch
    /// (`systems::calls`): the chat's call key and who sends under which
    /// index. `None` before the engine exists or for a chat this device is not in.
    #[must_use]
    pub fn call_epoch(&self, group: &[u8]) -> Option<cartopolis_e2ee::CallEpoch> {
        self.engine
            .as_ref()?
            .device
            .call_epoch(group, cartopolis_calls::keys::BASE_KEY_LEN)
            .ok()
    }
}

/// What an agent reading the chat over MCP needs to know about a private chat
/// (`systems::mcp::agent`) — which chat is the direct one with a person, and
/// which are groups whose names somebody else chose.
#[cfg(mcp)]
impl PrivateChats {
    /// See [`Engine::direct_with`]. `None` too when there is no engine, i.e.
    /// before a session has come up.
    #[must_use]
    pub fn direct_with(&self, username: &str) -> Option<Vec<u8>> {
        self.engine.as_ref()?.direct_with(username).cloned()
    }

    /// See [`Engine::describe`].
    #[must_use]
    pub fn describe(&self, group: &[u8]) -> Option<(bool, String)> {
        self.engine
            .as_ref()?
            .describe(group)
            .map(|(is_group, label)| (is_group, label.to_string()))
    }
}

/// A live share sends at most this often while the phone moves…
const SHARE_MIN_INTERVAL_MS: u64 = 10_000;
/// …and only once it has moved this far, in metres…
const SHARE_MIN_MOVE_M: f64 = 15.0;
/// …but at least this often when standing still, so the other side can tell a
/// quiet share from a dead phone.
const SHARE_HEARTBEAT_MS: u64 = 60_000;

/// Whether a share should send now, given what it last sent.
fn share_due(last: Option<(u64, f64, f64)>, now_ms: u64, lat: f64, lng: f64) -> bool {
    let Some((at, plat, plng)) = last else {
        return true;
    };
    let waited = now_ms.saturating_sub(at);
    waited >= SHARE_HEARTBEAT_MS
        || (waited >= SHARE_MIN_INTERVAL_MS
            && metres_between(plat, plng, lat, lng) >= SHARE_MIN_MOVE_M)
}

/// Ground distance, equirectangular: exact enough over the metres a share
/// compares, and free of the trigonometry a great-circle formula spends.
fn metres_between(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    const R: f64 = 6_371_000.0;
    let x = (lng2 - lng1).to_radians() * ((lat1 + lat2) / 2.0).to_radians().cos();
    let y = (lat2 - lat1).to_radians();
    (x * x + y * y).sqrt() * R
}

/// Run this phone's location shares for one frame: end the ones whose time is
/// up (telling their chats), hold the location watch while any is live, and
/// send each chat an update when [`share_due`] says so.
fn share_location(
    engine: &mut Engine,
    private: &mut PrivateChats,
    chat: &mut crate::systems::chat::ChatState,
    out: &mut Vec<NetMessage>,
    events: &mut Vec<Event>,
) {
    use crate::platform::location::LocationWatch;
    let now = now_ms();
    chat.expire_shares(now);
    for channel in std::mem::take(&mut chat.share_stops) {
        private.last_sent.remove(&channel);
        if let Some(group) = group_of(&channel) {
            if let Err(e) = engine.send_location_stopped(&group, now, out) {
                tracing::warn!(error = %e, "could not tell a chat sharing stopped");
            }
        }
    }
    if chat.sharing.is_empty() {
        private.watch = None;
        return;
    }
    if !LocationWatch::available() {
        // Desktop has no location provider (`platform::location`). Said once, and
        // the share is dropped rather than left promising something it cannot do.
        chat.sharing.clear();
        events.push(Event::Notice(
            "This device can't report where it is. Share your location from the phone app or \
             the web version."
                .into(),
        ));
        private.watch = None;
        return;
    }
    let watch = private.watch.get_or_insert_with(LocationWatch::start);
    if let Some(err) = watch.error() {
        chat.sharing.clear();
        events.push(Event::Notice(format!("Location sharing stopped: {err}")));
        private.watch = None;
        return;
    }
    let Some(fix) = watch.latest() else {
        return; // still waiting for the first fix
    };
    for share in chat.sharing.clone() {
        let last = private.last_sent.get(&share.channel).copied();
        if !share_due(last, now, fix.lat, fix.lng) {
            continue;
        }
        let Some(group) = group_of(&share.channel) else {
            continue;
        };
        match engine.send_location(
            &group,
            (fix.lat, fix.lng, fix.accuracy_m as f32),
            share.until_ms,
            now,
            out,
        ) {
            Ok(()) => {
                private
                    .last_sent
                    .insert(share.channel.clone(), (now, fix.lat, fix.lng));
                private.dirty = true;
            }
            Err(e) => tracing::warn!(error = %e, "could not send a location update"),
        }
    }
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
            chat.open_channel(&channel_key(g), &private_label(peer));
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
    for (name, members) in std::mem::take(&mut chat.group_start) {
        engine.start_group(&name, &members, &mut out, &mut events);
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
    share_location(&mut engine, &mut private, &mut chat, &mut out, &mut events);
    for (channel, point) in std::mem::take(&mut chat.private_points) {
        let Some(group) = group_of(&channel) else {
            continue;
        };
        let link = Some(match point.view {
            Some(v) => crate::systems::place_link::web_link(
                point.latitude,
                point.longitude,
                v.altitude,
                Some((v.pitch, v.yaw)),
            ),
            None => {
                crate::systems::place_link::web_link(point.latitude, point.longitude, 300.0, None)
            }
        });
        match engine.send_point(&group, &point, link, now_ms(), &mut out) {
            Ok(()) => {
                let me = engine.username().to_string();
                let my_id = chat.my_id.unwrap_or(0);
                chat.receive_point(
                    &channel,
                    my_id,
                    me,
                    point_caption(&point),
                    None,
                    Some(point),
                );
            }
            Err(e) => events.push(Event::Notice(format!("not sent: {e}"))),
        }
        private.dirty = true;
    }
    for ev in events {
        match ev {
            Event::Opened { group, peer } => {
                let key = channel_key(&group);
                chat.open_channel(&key, &private_label(&peer));
                chat.focus_channel(&key);
            }
            Event::Line {
                group,
                from,
                body,
                point,
                ..
            } => {
                let filter = private
                    .filter
                    .get_or_insert_with(cartopolis_core::Filter::seeded);
                let (body, _) = filter.mask(&body);
                // A point's own text is masked too: it is shown in the same log.
                let point = point.map(|mut p| {
                    p.text = p.text.map(|t| filter.mask(&t).0);
                    p.name = p.name.map(|n| filter.mask(&n).0);
                    p
                });
                chat.receive_point(&channel_key(&group), 0, from, body, None, point);
            }
            Event::Location {
                group,
                from,
                latitude,
                longitude,
                accuracy_m,
                until_ms,
            } => chat.update_location(
                crate::systems::chat::SharedLocation {
                    channel: channel_key(&group),
                    from,
                    latitude,
                    longitude,
                    accuracy_m,
                    until_ms,
                },
                now_ms(),
            ),
            Event::LocationStopped { group, from } => {
                chat.drop_location(&channel_key(&group), &from);
            }
            Event::Named { group, name } => {
                chat.open_channel(&channel_key(&group), &private_label(&name));
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
    fn a_private_label_carries_the_padlock_and_gives_back_the_name() {
        let label = private_label("bob");
        assert!(label.starts_with(crate::systems::icons::LOCK));
        assert_eq!(peer_of_label(&label), "bob");
        assert_eq!(peer_of_label("Global"), "Global");
    }

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
    fn a_share_sends_first_then_on_movement_then_as_a_heartbeat() {
        let (lat, lng) = (53.2, 6.5);
        assert!(share_due(None, 0, lat, lng), "the first fix goes at once");
        let last = Some((0, lat, lng));
        assert!(
            !share_due(last, 5_000, lat + 0.001, lng),
            "too soon, even having moved"
        );
        assert!(
            share_due(last, 12_000, lat + 0.001, lng),
            "~110 m after 12 s"
        );
        assert!(!share_due(last, 30_000, lat, lng), "standing still");
        assert!(
            share_due(last, 61_000, lat, lng),
            "heartbeat after a minute"
        );
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
