//! The private-chat engine for the browser (`docs/notes/web-chat.md`).
//!
//! [`ChatCore`] is [`cartopolis_chat::Engine`] with a byte-shaped surface a
//! JavaScript caller can drive: wire frames in ([`ChatCore::receive`]), wire
//! frames out ([`ChatCore::next_frame`]), what happened as JSON
//! ([`ChatCore::take_events`]), and the device's state as one blob to keep
//! ([`ChatCore::take_state`]). It holds no socket and no storage — the shared
//! worker that loads it owns both — so the same calls are exercised natively by
//! the tests at the bottom.
//!
//! **One per browser.** The state blob is a device's private keys and the
//! position of every chat's ratchet; two copies of it advancing independently
//! and saving over each other break the groups they are in, with nothing to say
//! so. That is why the web UI runs this in a `SharedWorker` and never in a page.

use std::collections::VecDeque;

use cartopolis_chat::{channel_key, group_of, Engine, Event};
use cartopolis_core::point::Point;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, CommunityDto, CommunityListing,
    CommunityMemberDto, CommunityRole, CommunitySettings, DirectoryOrder, JoinRule, ListingState,
    NetMessage, ReportReason, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

/// The request id this module sends `SetPresence` under. Outside the private
/// range (`cartopolis_chat::REQUEST_BASE`), so a refusal of it is never handed
/// to the engine.
const PRESENCE_REQUEST: u32 = 1;

/// What a call refuses with. A plain string here, so the native tests can
/// hold one: building a `JsError` calls into JavaScript, which does not exist
/// off wasm. It becomes a JavaScript `Error` only at the boundary.
#[derive(Debug)]
pub struct Error(String);

impl From<Error> for JsValue {
    fn from(e: Error) -> Self {
        JsError::new(&e.0).into()
    }
}

fn js_err(e: impl std::fmt::Display) -> Error {
    Error(e.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One device's private chats over one connection.
#[wasm_bindgen]
pub struct ChatCore {
    engine: Option<Engine>,
    /// Who the server says we are, from its `ServerHelloAck`.
    username: Option<String>,
    /// The same person as a full address (`username@server_name`, protocol
    /// 37): what the device's credential and every chat event carry.
    address: Option<String>,
    out: VecDeque<Vec<u8>>,
    events: Vec<Value>,
    dirty: bool,
}

impl Default for ChatCore {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl ChatCore {
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            engine: None,
            username: None,
            address: None,
            out: VecDeque::new(),
            events: Vec::new(),
            dirty: false,
        }
    }

    /// The first frame of a connection: sign in with a session token, the one
    /// the web UI already holds from signing in over HTTP.
    pub fn hello_token(token: &str) -> Result<Vec<u8>, Error> {
        encode_app_frame(&NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: AuthCredential::Token(token.to_string()),
        })
        .map_err(js_err)
    }

    /// The first frame of a connection, signing in with a password. For tests
    /// and development; the web UI signs in over HTTP and uses a token.
    pub fn hello_password(username: &str, password: &str) -> Result<Vec<u8>, Error> {
        encode_app_frame(&NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: AuthCredential::Password {
                username: username.to_string(),
                password: password.to_string(),
            },
        })
        .map_err(js_err)
    }

    /// The protocol version this module speaks. The server refuses any other.
    #[must_use]
    pub fn protocol_version() -> u16 {
        PROTOCOL_VERSION
    }

    /// The account the server signed this connection in as, once it has.
    #[must_use]
    pub fn username(&self) -> Option<String> {
        self.username.clone()
    }

    /// The same, as a full address (`name@server`).
    #[must_use]
    pub fn address(&self) -> Option<String> {
        self.address.clone()
    }

    /// Whether [`Self::open`] has run, i.e. whether chats can be used.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.engine.is_some()
    }

    /// One frame from the server. Anything that is not about signing in or
    /// about private chats is ignored, as is a frame this build cannot decode
    /// (a newer server's message): neither is an error worth closing over.
    pub fn receive(&mut self, frame: &[u8]) -> Result<(), Error> {
        let Ok(msg) = decode_app_frame(frame) else {
            return Ok(());
        };
        match msg {
            NetMessage::ServerHelloAck {
                username,
                session_token,
                server_name,
                ..
            } => {
                let address = cartopolis_core::address::person(&username, &server_name);
                // Nobody is looking at a world through this connection, so the
                // server need not send it twenty snapshots a second. Chat and
                // private-chat traffic still arrive.
                self.push(&NetMessage::SetPresence {
                    request_id: PRESENCE_REQUEST,
                    active: false,
                })?;
                self.events.push(json!({
                    "kind": "signed_in",
                    "username": username,
                    "address": address,
                    "server_name": server_name,
                    "session_token": session_token,
                }));
                self.username = Some(username);
                self.address = Some(address);
            }
            NetMessage::ServerError {
                message,
                code,
                request_id,
            } if self.username.is_none() => {
                // Before the ack, a refusal is about the handshake: a bad token,
                // a disabled account, a version this server does not speak.
                self.events.push(json!({
                    "kind": "refused",
                    "code": code,
                    "request_id": request_id,
                    "message": message,
                }));
            }
            msg if cartopolis_chat::is_private_message(&msg) => {
                if let Some(engine) = self.engine.as_mut() {
                    let mut out = Vec::new();
                    let mut events = Vec::new();
                    engine.handle(msg, &mut out, &mut events);
                    self.dirty = true;
                    self.flush(out, events)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Start chatting as the signed-in account: from `saved` (what
    /// [`Self::take_state`] last returned for this account), or as a brand-new
    /// device when there is nothing saved.
    ///
    /// A saved blob that does not load is an error, never a silent new device:
    /// starting over would orphan every chat that blob was in, and the caller
    /// is the one who can decide to.
    pub fn open(&mut self, saved: Option<Vec<u8>>) -> Result<(), Error> {
        let address = self
            .address
            .clone()
            .ok_or_else(|| Error("not signed in yet".into()))?;
        let mut engine = match saved {
            Some(bytes) => Engine::load(&bytes).map_err(js_err)?,
            None => Engine::new(&address).map_err(js_err)?,
        };
        if !engine.address().eq_ignore_ascii_case(&address) {
            return Err(Error("the saved keys belong to another account".into()));
        }
        let mut out = Vec::new();
        engine.on_connected(&mut out);
        self.engine = Some(engine);
        self.dirty = true;
        self.flush(out, Vec::new())
    }

    /// The next frame to send, until there are none.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        self.out.pop_front()
    }

    /// What happened since the last call, as a JSON array (see the module's
    /// TypeScript description in `crates/atlas/web/src/worker/chat-core.ts`).
    pub fn take_events(&mut self) -> String {
        Value::Array(std::mem::take(&mut self.events)).to_string()
    }

    /// The device's state, when it has changed since the last call — to be
    /// written to storage before anything else is sent. It holds private keys.
    pub fn take_state(&mut self) -> Option<Vec<u8>> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        self.engine.as_ref().map(Engine::save)
    }

    /// Every chat this device is in, as a JSON array of
    /// `{channel, label, group, people}`.
    pub fn chats(&self) -> String {
        let Some(engine) = self.engine.as_ref() else {
            return "[]".into();
        };
        let list: Vec<Value> = engine
            .chats()
            .map(|(group, label)| {
                let is_group = engine.describe(group).is_some_and(|(g, _)| g);
                json!({
                    "channel": channel_key(group),
                    "label": label,
                    "group": is_group,
                    "people": engine.people_in(group),
                })
            })
            .collect();
        Value::Array(list).to_string()
    }

    /// Open (or find) the direct chat with `username`.
    pub fn start_direct(&mut self, username: &str) -> Result<(), Error> {
        let engine = self.engine_mut()?;
        let (mut out, mut events) = (Vec::new(), Vec::new());
        engine.start_direct(username, &mut out, &mut events);
        self.dirty = true;
        self.flush(out, events)
    }

    /// Start a group chat called `name` with `members` (usernames).
    pub fn start_group(&mut self, name: &str, members: Vec<String>) -> Result<(), Error> {
        let engine = self.engine_mut()?;
        let (mut out, mut events) = (Vec::new(), Vec::new());
        engine.start_group(name, &members, &mut out, &mut events);
        self.dirty = true;
        self.flush(out, events)
    }

    /// Say `body` in the chat `channel` (a `dm:…` key from [`Self::chats`]).
    /// Answers the request id; a `sent` event later names it with the
    /// message's id (its place in the log).
    pub fn send_text(&mut self, channel: &str, body: &str, now_ms: f64) -> Result<u32, Error> {
        self.send(channel, |e, g, out| {
            e.send_text(g, body, now_ms as u64, out)
        })
    }

    /// Send a point — a place and the view it was seen from — given as the
    /// ActivityStreams JSON the app's points are (`cartopolis_core::point`).
    pub fn send_point(
        &mut self,
        channel: &str,
        point_json: &str,
        now_ms: f64,
    ) -> Result<u32, Error> {
        let point = Point::from_activity_json(point_json).map_err(js_err)?;
        self.send(channel, |e, g, out| {
            e.send_point(g, &point, None, now_ms as u64, out)
        })
    }

    /// Say `body` in the thread under the message `parent`.
    pub fn send_reply(
        &mut self,
        channel: &str,
        parent: f64,
        body: &str,
        now_ms: f64,
    ) -> Result<u32, Error> {
        self.send(channel, |e, g, out| {
            e.send_reply(g, parent as u64, body, now_ms as u64, out)
        })
    }

    /// React to the message `target` with `emoji`, or take it back.
    pub fn send_reaction(
        &mut self,
        channel: &str,
        target: f64,
        emoji: &str,
        remove: bool,
        now_ms: f64,
    ) -> Result<u32, Error> {
        self.send(channel, |e, g, out| {
            e.send_reaction(g, target as u64, emoji, remove, now_ms as u64, out)
        })
    }

    /// Replace the text of this person's own message `target`.
    pub fn send_edit(
        &mut self,
        channel: &str,
        target: f64,
        body: &str,
        now_ms: f64,
    ) -> Result<u32, Error> {
        self.send(channel, |e, g, out| {
            e.send_edit(g, target as u64, body, now_ms as u64, out)
        })
    }

    /// Take back this person's own message `target`.
    pub fn send_delete(&mut self, channel: &str, target: f64, now_ms: f64) -> Result<u32, Error> {
        self.send(channel, |e, g, out| {
            e.send_delete(g, target as u64, now_ms as u64, out)
        })
    }

    // ── Communities and topics (protocol 39) ───────────────────────────────
    //
    // Answers arrive as a `communities` event (the whole list each time), a
    // `directory` event, or a `notice` when refused. Ids are uuid strings,
    // topics are named by the same `dm:<hex>` key their chats have.

    /// Ask for every community this account is in or waiting for.
    pub fn list_communities(&mut self) -> Result<(), Error> {
        self.call(|e, out, _| e.list_communities(out))
    }

    /// Start a community: `settings` is `{name, about, join, listed,
    /// members_start_topics}` as JSON (`join` one of open/ask/invite), with
    /// its first topics and people.
    pub fn create_community(
        &mut self,
        settings: &str,
        topics: Vec<String>,
        people: Vec<String>,
    ) -> Result<(), Error> {
        let s = settings_of(settings)?;
        self.dirty = true;
        self.call(|e, out, _| e.create_community(s, &topics, &people, out))
    }

    /// A moderator changes a community's settings (same JSON as creating).
    pub fn edit_community(&mut self, id: &str, settings: &str) -> Result<(), Error> {
        let (id, s) = (community_id(id)?, settings_of(settings)?);
        self.call(|e, out, _| e.edit_community(id, s, out))
    }

    /// Search the listed communities; `order` is `popular` or `new`.
    pub fn find_communities(&mut self, query: &str, order: &str) -> Result<(), Error> {
        let order = if order == "new" {
            DirectoryOrder::New
        } else {
            DirectoryOrder::Popular
        };
        self.call(|e, out, _| e.find_communities(query, order, out))
    }

    /// Join an open community, or ask to join one that asks first.
    pub fn join_community(&mut self, id: &str, message: &str) -> Result<(), Error> {
        let id = community_id(id)?;
        self.call(|e, out, _| e.join_community(id, message, out))
    }

    /// Leave a community, or withdraw a request to join it.
    pub fn leave_community(&mut self, id: &str) -> Result<(), Error> {
        let id = community_id(id)?;
        self.call(|e, out, _| e.leave_community(id, out))
    }

    /// A moderator lets somebody in, or turns them down.
    pub fn decide_request(&mut self, id: &str, who: &str, accept: bool) -> Result<(), Error> {
        let id = community_id(id)?;
        self.call(|e, out, _| e.decide_request(id, who, accept, out))
    }

    /// A moderator adds somebody to a community.
    pub fn add_to_community(&mut self, id: &str, who: &str) -> Result<(), Error> {
        let id = community_id(id)?;
        self.call(|e, out, _| e.add_to_community(id, who, out))
    }

    /// The owner sets somebody's role: `owner` (hand over), `moderator` or
    /// `member`.
    pub fn set_community_role(&mut self, id: &str, who: &str, role: &str) -> Result<(), Error> {
        let id = community_id(id)?;
        let role = match role {
            "owner" => CommunityRole::Owner,
            "moderator" => CommunityRole::Moderator,
            _ => CommunityRole::Member,
        };
        self.call(|e, out, _| e.set_community_role(id, who, role, out))
    }

    /// A moderator takes somebody out of one topic (`channel`, or empty for
    /// the whole community), and with `block` stops them asking again.
    pub fn remove_from_community(
        &mut self,
        id: &str,
        who: &str,
        channel: &str,
        block: bool,
    ) -> Result<(), Error> {
        let id = community_id(id)?;
        let topic = group_of(channel);
        self.call(|e, out, _| e.remove_from_community(id, who, topic.as_deref(), block, out))
    }

    /// The owner deletes a community.
    pub fn delete_community(&mut self, id: &str) -> Result<(), Error> {
        let id = community_id(id)?;
        self.call(|e, out, _| e.delete_community(id, out))
    }

    /// Start a topic in a community.
    pub fn start_topic(&mut self, id: &str, name: &str) -> Result<(), Error> {
        let id = community_id(id)?;
        self.dirty = true;
        self.call(|e, out, events| e.start_topic(id, name, out, events))
    }

    /// A moderator renames, moves or archives a topic.
    pub fn edit_topic(
        &mut self,
        id: &str,
        channel: &str,
        name: &str,
        position: u32,
        archived: bool,
    ) -> Result<(), Error> {
        let id = community_id(id)?;
        let group = group_of(channel).ok_or_else(|| Error("not a topic".into()))?;
        self.call(|e, out, _| e.edit_topic(id, &group, name, position, archived, out))
    }

    /// A moderator deletes message `target` from a topic, for everybody.
    pub fn moderate_delete(&mut self, channel: &str, target: f64) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a topic".into()))?;
        self.call(|e, out, _| e.moderate_delete(&group, target as u64, out))
    }

    /// Report message `target` in a topic to the community's moderators.
    /// `reason` is one of the report reasons' stable names (`spam`, …).
    pub fn report_to_moderators(
        &mut self,
        channel: &str,
        target: f64,
        who: &str,
        reason: &str,
        note: &str,
        line: &str,
    ) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a topic".into()))?;
        let engine = self.engine_mut()?;
        let id = engine
            .community_of(&group)
            .map(|c| c.id)
            .ok_or_else(|| Error("not a topic of a community".into()))?;
        let reason = ReportReason::from_wire(reason);
        self.call(|e, out, _| {
            e.report_to_moderators(id, who, &group, target as u64, reason, note, line, out);
        })
    }

    /// A moderator closes a report.
    pub fn resolve_report(&mut self, id: &str, report: f64) -> Result<(), Error> {
        let id = community_id(id)?;
        self.call(|e, out, _| e.resolve_report(id, report as i64, out))
    }

    /// Add somebody to a group chat this device is in.
    pub fn add_to_chat(&mut self, channel: &str, who: &str) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        let engine = self.engine_mut()?;
        let (mut out, mut events) = (Vec::new(), Vec::new());
        engine.add_to_chat(&group, who, &mut out, &mut events);
        self.dirty = true;
        self.flush(out, events)
    }
}

fn community_id(s: &str) -> Result<uuid::Uuid, Error> {
    uuid::Uuid::parse_str(s).map_err(|_| Error(format!("{s:?} is not a community id")))
}

fn rule_name(r: JoinRule) -> &'static str {
    match r {
        JoinRule::Open => "open",
        JoinRule::Ask => "ask",
        JoinRule::Invite => "invite",
    }
}

fn role_name(r: CommunityRole) -> &'static str {
    match r {
        CommunityRole::Owner => "owner",
        CommunityRole::Moderator => "moderator",
        CommunityRole::Member => "member",
    }
}

/// Settings as a page writes them.
fn settings_of(json: &str) -> Result<CommunitySettings, Error> {
    let v: Value = serde_json::from_str(json).map_err(|e| Error(e.to_string()))?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    Ok(CommunitySettings {
        name: text("name"),
        about: text("about"),
        join: match text("join").as_str() {
            "open" => JoinRule::Open,
            "ask" => JoinRule::Ask,
            _ => JoinRule::Invite,
        },
        listed: flag("listed"),
        members_start_topics: flag("members_start_topics"),
    })
}

fn settings_json(s: &CommunitySettings) -> Value {
    json!({
        "name": s.name,
        "about": s.about,
        "join": rule_name(s.join),
        "listed": s.listed,
        "members_start_topics": s.members_start_topics,
    })
}

fn member_json(m: &CommunityMemberDto) -> Value {
    json!({"address": m.address, "role": role_name(m.role)})
}

/// A community as the JSON the worker hands to pages: topics by the same
/// `dm:<hex>` key their chats have, so a page needs one key for both.
fn community_json(c: CommunityDto) -> Value {
    json!({
        "id": c.id.to_string(),
        "home": c.home,
        "settings": settings_json(&c.settings),
        "role": c.role.map(role_name),
        "members": c.members.iter().map(member_json).collect::<Vec<_>>(),
        "topics": c.topics.into_iter().map(|t| json!({
            "channel": channel_key(&t.group),
            "name": t.name,
            "position": t.position,
            "archived": t.archived,
            "joined": t.joined,
        })).collect::<Vec<_>>(),
        "requests": c.requests.into_iter().map(|r| json!({
            "address": r.address,
            "message": r.message,
            "requested_ms": r.requested_ms,
        })).collect::<Vec<_>>(),
        "reports": c.reports.into_iter().map(|r| json!({
            "id": r.id,
            "reporter": r.reporter,
            "reported": r.reported,
            "channel": channel_key(&r.group),
            "seq": r.seq,
            "reason": r.reason.as_str(),
            "note": r.note,
            "line": r.line,
            "created_ms": r.created_ms,
        })).collect::<Vec<_>>(),
    })
}

fn listing_json(l: CommunityListing) -> Value {
    json!({
        "id": l.id.to_string(),
        "name": l.name,
        "about": l.about,
        "join": rule_name(l.join),
        "members": l.members,
        "topics": l.topics,
        "moderators": l.moderators.iter().map(member_json).collect::<Vec<_>>(),
        "mine": match l.mine {
            ListingState::Outside => "outside",
            ListingState::Asked => "asked",
            ListingState::Member => "member",
            ListingState::Refused => "refused",
        },
        "created_ms": l.created_ms,
    })
}

impl ChatCore {
    /// One engine call that may queue frames and events.
    fn call(
        &mut self,
        f: impl FnOnce(&mut Engine, &mut Vec<NetMessage>, &mut Vec<Event>),
    ) -> Result<(), Error> {
        let engine = self.engine_mut()?;
        let (mut out, mut events) = (Vec::new(), Vec::new());
        f(engine, &mut out, &mut events);
        self.flush(out, events)
    }
}

impl ChatCore {
    /// One send into `channel`, by whichever of the engine's send calls `f`
    /// makes: its frames queued, the state marked to be saved, its request id
    /// answered.
    fn send(
        &mut self,
        channel: &str,
        f: impl FnOnce(&mut Engine, &[u8], &mut Vec<NetMessage>) -> Result<u32, String>,
    ) -> Result<u32, Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        let engine = self.engine_mut()?;
        let mut out = Vec::new();
        let request = f(engine, &group, &mut out).map_err(js_err)?;
        self.dirty = true;
        self.flush(out, Vec::new())?;
        Ok(request)
    }

    fn engine_mut(&mut self) -> Result<&mut Engine, Error> {
        self.engine
            .as_mut()
            .ok_or_else(|| Error("chats are not open yet".into()))
    }

    fn push(&mut self, msg: &NetMessage) -> Result<(), Error> {
        self.out.push_back(encode_app_frame(msg).map_err(js_err)?);
        Ok(())
    }

    fn flush(&mut self, out: Vec<NetMessage>, events: Vec<Event>) -> Result<(), Error> {
        for m in &out {
            self.push(m)?;
        }
        self.events.extend(events.into_iter().map(event_json));
        Ok(())
    }
}

/// An engine event as the JSON the worker hands to pages. Group ids travel as
/// the same `dm:<hex>` channel key the app uses.
fn event_json(event: Event) -> Value {
    match event {
        Event::Opened { group, peer } => {
            json!({ "kind": "opened", "channel": channel_key(&group), "peer": peer })
        }
        Event::Line {
            group,
            seq,
            from,
            body,
            point,
            picture,
            sent_at_ms,
            parent,
        } => json!({
            "kind": "line",
            "channel": channel_key(&group),
            "from": from,
            "body": body,
            "point": point.and_then(|p| p.to_activity_json(None).ok()),
            "picture": picture.map(|p| json!({
                "hash": p.hash,
                "key": hex(&p.key),
                "nonce": hex(&p.nonce),
                "width": p.width,
                "height": p.height,
            })),
            "seq": seq,
            "parent": parent,
            "sent_at_ms": sent_at_ms,
        }),
        Event::Location {
            group,
            from,
            latitude,
            longitude,
            accuracy_m,
            until_ms,
        } => json!({
            "kind": "location",
            "channel": channel_key(&group),
            "from": from,
            "latitude": latitude,
            "longitude": longitude,
            "accuracy_m": accuracy_m,
            "until_ms": until_ms,
        }),
        Event::LocationStopped { group, from } => {
            json!({ "kind": "location_stopped", "channel": channel_key(&group), "from": from })
        }
        Event::Named { group, name } => {
            json!({ "kind": "named", "channel": channel_key(&group), "name": name })
        }
        Event::Notice(text) => json!({ "kind": "notice", "text": text }),
        Event::Reaction {
            group,
            from,
            target,
            emoji,
            remove,
        } => json!({
            "kind": "reaction",
            "channel": channel_key(&group),
            "from": from,
            "target": target,
            "emoji": emoji,
            "remove": remove,
        }),
        Event::Edited {
            group,
            from,
            target,
            body,
        } => json!({
            "kind": "edited",
            "channel": channel_key(&group),
            "from": from,
            "target": target,
            "body": body,
        }),
        Event::Deleted {
            group,
            from,
            target,
        } => json!({
            "kind": "deleted",
            "channel": channel_key(&group),
            "from": from,
            "target": target,
        }),
        Event::Sent {
            group,
            request,
            seq,
        } => json!({
            "kind": "sent",
            "channel": channel_key(&group),
            "request": request,
            "seq": seq,
        }),
        Event::Communities(list) => json!({
            "kind": "communities",
            "communities": list.into_iter().map(community_json).collect::<Vec<_>>(),
        }),
        Event::CommunityCreated(id) => json!({
            "kind": "community_created",
            "id": id.to_string(),
        }),
        Event::Directory(list) => json!({
            "kind": "directory",
            "listings": list.into_iter().map(listing_json).collect::<Vec<_>>(),
        }),
        Event::Redacted { group, seq, by } => json!({
            "kind": "redacted",
            "channel": channel_key(&group),
            "seq": seq,
            "by": by,
        }),
        Event::Left {
            group,
            community,
            by,
        } => json!({
            "kind": "left",
            "channel": channel_key(&group),
            "community": community.to_string(),
            "by": by,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ack(username: &str) -> Vec<u8> {
        encode_app_frame(&NetMessage::ServerHelloAck {
            session_id: Default::default(),
            tick_hz: 20.0,
            your_avatar_id: 1,
            principal_id: Default::default(),
            username: username.into(),
            session_token: "token".into(),
            osm_tile_url_template: String::new(),
            api_base_url: String::new(),
            server_name: "test.example".into(),
        })
        .unwrap()
    }

    fn sent(core: &mut ChatCore) -> Vec<NetMessage> {
        std::iter::from_fn(|| core.next_frame())
            .map(|f| decode_app_frame(&f).unwrap())
            .collect()
    }

    fn events(core: &mut ChatCore) -> Vec<Value> {
        serde_json::from_str(&core.take_events()).unwrap()
    }

    #[test]
    fn signing_in_turns_the_world_off_and_says_who() {
        let mut core = ChatCore::new();
        core.receive(&ack("ann")).unwrap();
        assert_eq!(core.username().as_deref(), Some("ann"));
        assert!(matches!(
            sent(&mut core).as_slice(),
            [NetMessage::SetPresence { active: false, .. }]
        ));
        let ev = events(&mut core);
        assert_eq!(ev[0]["kind"], "signed_in");
        assert_eq!(ev[0]["username"], "ann");
        assert_eq!(ev[0]["address"], "ann@test.example");
        assert!(!core.is_open(), "chats wait for the caller to open them");
    }

    #[test]
    fn a_refused_handshake_is_an_event_not_a_crash() {
        let mut core = ChatCore::new();
        let refusal = encode_app_frame(&NetMessage::ServerError {
            request_id: 0,
            code: 401,
            message: "bad token".into(),
        })
        .unwrap();
        core.receive(&refusal).unwrap();
        let ev = events(&mut core);
        assert_eq!(ev[0]["kind"], "refused");
        assert_eq!(ev[0]["message"], "bad token");
    }

    #[test]
    fn opening_registers_the_device_and_its_state_comes_back() {
        let mut core = ChatCore::new();
        core.receive(&ack("ann")).unwrap();
        let _ = sent(&mut core);
        core.open(None).unwrap();
        let out = sent(&mut core);
        assert!(out
            .iter()
            .any(|m| matches!(m, NetMessage::E2eeRegisterDevice { .. })));
        let state = core
            .take_state()
            .expect("a new device is something to keep");
        assert!(core.take_state().is_none(), "nothing changed since");

        // The same keys, on the next connection.
        let mut again = ChatCore::new();
        again.receive(&ack("ann")).unwrap();
        again.open(Some(state.clone())).unwrap();
        assert!(again.is_open());

        // And never somebody else's.
        let mut other = ChatCore::new();
        other.receive(&ack("bob")).unwrap();
        assert!(other.open(Some(state)).is_err());
    }

    #[test]
    fn nothing_opens_before_the_server_says_who_we_are() {
        let mut core = ChatCore::new();
        assert!(core.open(None).is_err());
        assert!(core.start_direct("bob").is_err());
        assert_eq!(core.chats(), "[]");
    }
}
