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

use cartopolis_chat::{channel_key, group_of, Engine, Event, HistoryItem, PictureRef};
use cartopolis_core::meeting::MeetingObject;
use cartopolis_core::point::Point;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, CallAction, CallEndReason, CallRole,
    CallTrackKind, CommunityDto, CommunityListing, CommunityMemberDto, CommunityPlace,
    CommunityRole, CommunitySettings, DirectoryOrder, JoinRule, ListingState, NetMessage,
    ReportReason, CALL_CONTROL_REQUEST, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

/// The request id this module sends `SetPresence` under. Outside the private
/// range (`cartopolis_chat::REQUEST_BASE`), so a refusal of it is never handed
/// to the engine.
const PRESENCE_REQUEST: u32 = 1;

/// Call joins go out under this range, as the app's do (`systems::calls`), so a
/// refusal of one is told apart from a private chat's.
const CALL_REQUEST_BASE: u32 = 0xCA11_0000;

/// Bytes of a call's SFrame base key (`cartopolis_calls::keys::BASE_KEY_LEN`).
const CALL_KEY_LEN: usize = 16;

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

fn unhex_vec(s: &str) -> Option<Vec<u8>> {
    (s.len() % 2 == 0).then_some(())?;
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect()
}

fn unhex<const N: usize>(s: &str) -> Result<[u8; N], Error> {
    let bytes: Option<Vec<u8>> = (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect();
    bytes
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| Error(format!("{s:?} is not {N} bytes of hex")))
}

fn picture_json(p: &PictureRef) -> Value {
    json!({
        "hash": p.hash,
        "key": hex(&p.key),
        "nonce": hex(&p.nonce),
        "width": p.width,
        "height": p.height,
    })
}

fn picture_of(v: &Value) -> Result<PictureRef, Error> {
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
    let num = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
    Ok(PictureRef {
        hash: text("hash").to_string(),
        key: unhex(text("key"))?,
        nonce: unhex(text("nonce"))?,
        width: num("width"),
        height: num("height"),
    })
}

/// A history packed for upload ([`pack_history`]): the sealed bytes, and the
/// key, nonce and count [`ChatCore::send_history`] announces with its hash.
#[wasm_bindgen]
pub struct PackedHistory {
    bytes: Vec<u8>,
    key: String,
    nonce: String,
    count: u32,
}

#[wasm_bindgen]
impl PackedHistory {
    #[wasm_bindgen(getter)]
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn key(&self) -> String {
        self.key.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn nonce(&self) -> String {
        self.nonce.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn count(&self) -> u32 {
        self.count
    }
}

/// Seal a chat's history for somebody just added (`history_wanted`). `items`
/// is a JSON array of `{seq, from, body, sent_at_ms, parent, point, picture}`,
/// oldest first; the newest `cartopolis_chat::MAX_HISTORY` are kept.
#[wasm_bindgen]
pub fn pack_history(items: &str) -> Result<PackedHistory, Error> {
    let list: Vec<Value> = serde_json::from_str(items).map_err(js_err)?;
    let mut out = Vec::new();
    for v in &list {
        out.push(HistoryItem {
            seq: v.get("seq").and_then(Value::as_u64).unwrap_or(0),
            from: v
                .get("from")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            body: v
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            sent_at_ms: v.get("sent_at_ms").and_then(Value::as_u64).unwrap_or(0),
            parent: v.get("parent").and_then(Value::as_u64),
            point: v.get("point").and_then(Value::as_str).map(str::to_string),
            picture: match v.get("picture") {
                Some(p) if p.is_object() => Some(picture_of(p)?),
                _ => None,
            },
        });
    }
    let (sealed, count) = cartopolis_chat::pack_history(&out).map_err(js_err)?;
    Ok(PackedHistory {
        key: hex(&sealed.key),
        nonce: hex(&sealed.nonce),
        bytes: sealed.bytes,
        count,
    })
}

/// Open a history fetched from the picture store, as the same JSON array
/// [`pack_history`] takes.
#[wasm_bindgen]
pub fn open_history(sealed: &[u8], hash: &str, key: &str, nonce: &str) -> Result<String, Error> {
    let picture = PictureRef {
        hash: hash.to_string(),
        key: unhex(key)?,
        nonce: unhex(nonce)?,
        width: 0,
        height: 0,
    };
    let items = cartopolis_chat::open_history(sealed, &picture).map_err(js_err)?;
    let list: Vec<Value> = items
        .into_iter()
        .map(|i| {
            json!({
                "seq": i.seq,
                "from": i.from,
                "body": i.body,
                "sent_at_ms": i.sent_at_ms,
                "parent": i.parent,
                "point": i.point,
                "picture": i.picture.as_ref().map(picture_json),
            })
        })
        .collect();
    Ok(Value::Array(list).to_string())
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

    /// The first frame of a guest's connection: a call link's code and the
    /// name they gave (protocol 41). The session token in the answer
    /// reconnects them for as long as the guest lasts.
    pub fn hello_guest(code: &str, name: &str) -> Result<Vec<u8>, Error> {
        encode_app_frame(&NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: AuthCredential::Guest {
                code: code.to_string(),
                name: name.to_string(),
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
            // Calls (protocol 26): signalling only, relayed to the page that owns
            // the call's media (`webcall.js`); the engine has no part in it.
            NetMessage::CallRoster {
                group,
                members,
                settings,
                waiting,
                spotlight,
                rooms,
            } => self.events.push(json!({
                "kind": "call_roster",
                "channel": channel_key(&group),
                "members": members.iter().map(|m| json!({
                    "participant": m.participant,
                    "username": m.username,
                    "device": m.device.to_string(),
                    "role": match m.role {
                        CallRole::Host => "host",
                        CallRole::CoHost => "cohost",
                        CallRole::Member => "member",
                    },
                    "hand": m.hand,
                    "held": m.held,
                    "sharing": m.sharing,
                    "muted": m.muted,
                    "recording": m.recording,
                    "guest": m.guest,
                    "room": m.room,
                    "agent": m.agent,
                })).collect::<Vec<_>>(),
                "settings": {
                    "locked": settings.locked,
                    "may_unmute": settings.may_unmute,
                    "may_share": settings.may_share,
                    "members_wait": settings.members_wait,
                    "may_record": settings.may_record,
                },
                "waiting": waiting.iter().map(|w| json!({
                    "participant": w.participant,
                    "username": w.username,
                    "guest": w.guest,
                    "since_ms": w.since_ms,
                })).collect::<Vec<_>>(),
                "spotlight": spotlight,
                "rooms": rooms.as_ref().map(|r| json!({
                    "names": r.names,
                    "until_ms": r.until_ms,
                    "may_move": r.may_move,
                })),
            })),
            NetMessage::CallWaiting {
                group, participant, ..
            } => self.events.push(json!({
                "kind": "call_waiting",
                "channel": channel_key(&group),
                "participant": participant,
            })),
            NetMessage::CallEnded { group, reason } => {
                let (why, by) = match reason {
                    CallEndReason::Removed { by } => ("removed", by),
                    CallEndReason::Ended { by } => ("ended", by),
                };
                self.events.push(json!({
                    "kind": "call_ended",
                    "channel": channel_key(&group),
                    "reason": why,
                    "by": by,
                }));
            }
            NetMessage::CallReacted {
                group,
                participant,
                epoch,
                sealed,
            } => self.events.push(json!({
                "kind": "call_reacted",
                "channel": channel_key(&group),
                "participant": participant,
                "epoch": epoch.to_string(),
                "sealed": hex(&sealed),
            })),
            // A guest link for this chat's call; or, with no code, the call a
            // guest that just signed in may join.
            NetMessage::CallInvited {
                group,
                code,
                expires_ms,
            } => self.events.push(json!({
                "kind": "call_invited",
                "channel": channel_key(&group),
                "code": code,
                "expires_ms": expires_ms,
            })),
            NetMessage::CallKeyDropped { group, sealed } => self.events.push(json!({
                "kind": "call_key_dropped",
                "channel": channel_key(&group),
                "sealed": hex(&sealed),
            })),
            NetMessage::CallJoined {
                group,
                participant,
                answer,
                ..
            } => self.events.push(json!({
                "kind": "call_joined",
                "channel": channel_key(&group),
                "participant": participant,
                "answer": answer,
            })),
            NetMessage::CallOffer { group, sdp, tracks } => self.events.push(json!({
                "kind": "call_offer",
                "channel": channel_key(&group),
                "sdp": sdp,
                "tracks": tracks.iter().map(|t| json!({
                    "mid": t.mid,
                    "participant": t.participant,
                    "kind": match t.kind {
                        CallTrackKind::Audio => "audio",
                        CallTrackKind::Camera => "camera",
                        CallTrackKind::Screen => "screen",
                    },
                    "video": t.kind != CallTrackKind::Audio,
                })).collect::<Vec<_>>(),
            })),
            NetMessage::CallDeclined { group, username } => self.events.push(json!({
                "kind": "call_declined",
                "channel": channel_key(&group),
                "username": username,
            })),
            NetMessage::ServerError {
                request_id,
                code,
                message,
            } if request_id & 0xFFFF_0000 == CALL_REQUEST_BASE => self.events.push(json!({
                "kind": "call_refused",
                "code": code,
                "message": message,
                // A refused host action, not the call failing (protocol 41).
                "control": request_id == CALL_CONTROL_REQUEST,
            })),
            // Who among this account's contacts is signed in, for the list's
            // dots. The server sends it whole at the handshake and again
            // whenever a contact comes or goes; `online` is null for somebody
            // who does not show it. Requests and settings are the app's.
            NetMessage::ContactsState { contacts, .. } => {
                let server = self
                    .address
                    .as_deref()
                    .and_then(cartopolis_core::address::split)
                    .map_or("", |(_, server)| server);
                self.events.push(json!({
                    "kind": "contacts",
                    "contacts": contacts.iter().map(|c| json!({
                        "address": cartopolis_core::address::person(&c.username, server),
                        "online": c.online,
                    })).collect::<Vec<_>>(),
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

    // ── Calls (protocol 26, `docs/notes/calls.md`) ──────────────────────────
    //
    // The media runs in the page (`webcall.js`): a worker has no
    // `RTCPeerConnection`. This only carries the signalling over the chat's
    // connection, and hands out the chat's call key for the current epoch.

    /// Join (or start) the call in `channel` with the page's sending offer.
    /// `screen` is the offer's media id for a shared screen (empty: none).
    pub fn call_join(&mut self, channel: &str, offer: &str, screen: &str) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        self.push(&NetMessage::CallJoin {
            request_id: CALL_REQUEST_BASE | 1,
            group,
            offer: offer.to_string(),
            screen: (!screen.is_empty()).then(|| screen.to_string()),
        })
    }

    /// A hand, a shared screen or a host's action in `channel`'s call, as
    /// `CallAction`'s JSON (`{"Hand":true}`, `{"Mute":{"participant":3}}`,
    /// `"MuteAll"`, …).
    pub fn call_control(&mut self, channel: &str, action: &str) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        let action: CallAction =
            serde_json::from_str(action).map_err(|e| Error(format!("not a call action: {e}")))?;
        self.push(&NetMessage::CallControl { group, action })
    }

    /// A reaction the page sealed (`call-events.js`) for `epoch` (decimal),
    /// as hex.
    pub fn call_react(&mut self, channel: &str, epoch: &str, sealed: &str) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        let epoch = epoch.parse().map_err(|_| Error("not an epoch".into()))?;
        let sealed = unhex_vec(sealed).ok_or_else(|| Error("not hex".into()))?;
        self.push(&NetMessage::CallReact {
            group,
            epoch,
            sealed,
        })
    }

    /// Ask for a guest link to this chat's call (`call_invited` answers).
    pub fn call_invite(&mut self, channel: &str) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        self.push(&NetMessage::CallInvite { group })
    }

    /// Hand the guest seated as `participant` the call's key, which the page
    /// sealed under the link's secret (`call-events.js`), as hex.
    pub fn call_key_drop(
        &mut self,
        channel: &str,
        participant: f64,
        sealed: &str,
    ) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        let sealed = unhex_vec(sealed).ok_or_else(|| Error("not hex".into()))?;
        self.push(&NetMessage::CallKeyDrop {
            group,
            participant: participant as u64,
            sealed,
        })
    }

    /// Whose cameras the page draws: a JSON list of participants, or `null`
    /// for everybody.
    pub fn call_watch(&mut self, channel: &str, cameras: &str) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        let cameras: Option<Vec<u64>> =
            serde_json::from_str(cameras).map_err(|e| Error(format!("not a list: {e}")))?;
        self.push(&NetMessage::CallWatch { group, cameras })
    }

    /// Answer the forwarder's offer for the page's receiving connection.
    pub fn call_answer(&mut self, channel: &str, sdp: &str) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        self.push(&NetMessage::CallAnswer {
            group,
            sdp: sdp.to_string(),
        })
    }

    /// Leave the call in `channel`.
    pub fn call_leave(&mut self, channel: &str) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        self.push(&NetMessage::CallLeave { group })
    }

    /// Decline a call ringing in `channel`.
    pub fn call_decline(&mut self, channel: &str) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        self.push(&NetMessage::CallDecline { group })
    }

    /// Register this browser's push subscription (protocol 29's message, with
    /// a browser's `webpush:` address — `simulator::push`), so a call rings it
    /// while the page is closed; an empty string takes it back.
    pub fn set_push_endpoint(&mut self, endpoint: &str) -> Result<(), Error> {
        let endpoint = (!endpoint.is_empty()).then(|| endpoint.to_owned());
        self.push(&NetMessage::SetPushEndpoint { endpoint })
    }

    /// The call key of `channel`'s current epoch, as JSON
    /// `{epoch, key (hex), index}`, or `null` when this device is not in it.
    /// The page re-reads it after every step: a commit moves the key at once.
    pub fn call_keys(&self, channel: &str) -> String {
        let keys = group_of(channel).and_then(|g| {
            self.engine
                .as_ref()
                .and_then(|e| e.call_epoch(&g, CALL_KEY_LEN).ok())
        });
        match keys {
            Some(k) => json!({
                "epoch": k.epoch,
                "key": hex(&k.base_key),
                "index": k.own_index,
            })
            .to_string(),
            None => "null".into(),
        }
    }

    /// Announce a history packed with [`pack_history`] and stored under
    /// `hash`, for somebody this device just added to `channel`.
    pub fn send_history(
        &mut self,
        channel: &str,
        hash: &str,
        key: &str,
        nonce: &str,
        count: u32,
        now_ms: f64,
    ) -> Result<u32, Error> {
        let stored = PictureRef {
            hash: hash.to_string(),
            key: unhex(key)?,
            nonce: unhex(nonce)?,
            width: 0,
            height: 0,
        };
        self.send(channel, |e, g, out| {
            e.send_history(g, &stored, count, now_ms as u64, out)
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

    /// Send something a meeting leaves in a chat — a plan, an answer, a poll,
    /// a vote, a note, a record — as its ActivityStreams JSON
    /// (`cartopolis_core::meeting`). Read and written again here, so what goes
    /// out is what every reader accepts.
    pub fn send_meeting(&mut self, channel: &str, json: &str, now_ms: f64) -> Result<u32, Error> {
        let object = MeetingObject::from_activity_json(json)
            .ok_or_else(|| js_err("not something a meeting leaves in a chat"))?;
        self.send(channel, |e, g, out| {
            e.send_meeting(g, &object, now_ms as u64, out)
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
    /// `order` is `popular`, `new` or `near`; `latitude`/`longitude` are where
    /// the person is for `near` (NaN when the page has no location).
    pub fn find_communities(
        &mut self,
        query: &str,
        order: &str,
        latitude: f64,
        longitude: f64,
    ) -> Result<(), Error> {
        let order = match order {
            "new" => DirectoryOrder::New,
            "near" => DirectoryOrder::Near,
            _ => DirectoryOrder::Popular,
        };
        let near = (latitude.is_finite() && longitude.is_finite()).then_some((latitude, longitude));
        self.call(|e, out, _| e.find_communities(query, order, near, out))
    }

    /// A moderator makes an invite link; the code arrives as an `invite` event.
    pub fn create_invite(&mut self, id: &str) -> Result<(), Error> {
        let id = community_id(id)?;
        self.call(|e, out, _| e.create_invite(id, out))
    }

    /// Look an invite up: the community arrives as a `directory` event.
    pub fn lookup_invite(&mut self, code: &str) -> Result<(), Error> {
        self.call(|e, out, _| e.lookup_invite(code, out))
    }

    /// Join a community with an invite.
    pub fn join_invite(&mut self, code: &str) -> Result<(), Error> {
        self.call(|e, out, _| e.join_invite(code, out))
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
        picture: v
            .get("picture")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .map(str::to_string),
        place: v.get("place").and_then(place_of),
    })
}

fn place_of(v: &Value) -> Option<CommunityPlace> {
    Some(CommunityPlace {
        latitude: v.get("latitude")?.as_f64()?,
        longitude: v.get("longitude")?.as_f64()?,
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn place_json(p: &CommunityPlace) -> Value {
    json!({"latitude": p.latitude, "longitude": p.longitude, "name": p.name})
}

fn settings_json(s: &CommunitySettings) -> Value {
    json!({
        "name": s.name,
        "about": s.about,
        "join": rule_name(s.join),
        "listed": s.listed,
        "members_start_topics": s.members_start_topics,
        "picture": s.picture,
        "place": s.place.as_ref().map(place_json),
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
        "picture": l.picture,
        "place": l.place.as_ref().map(place_json),
        "distance_m": l.distance_m,
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
            object,
            sent_at_ms,
            parent,
        } => json!({
            "kind": "line",
            "channel": channel_key(&group),
            "from": from,
            "body": body,
            // The object it carried, point or meeting (`HistoryItem::point`).
            "point": point.and_then(|p| p.to_activity_json(None).ok()).or(object),
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
        Event::HistoryWanted { group } => json!({
            "kind": "history_wanted",
            "channel": channel_key(&group),
        }),
        Event::History {
            group,
            from,
            picture,
            count,
        } => json!({
            "kind": "shared_history",
            "channel": channel_key(&group),
            "from": from,
            "picture": picture_json(&picture),
            "count": count,
        }),
        Event::Communities(list) => json!({
            "kind": "communities",
            "communities": list.into_iter().map(community_json).collect::<Vec<_>>(),
        }),
        Event::CommunityCreated(id) => json!({
            "kind": "community_created",
            "id": id.to_string(),
        }),
        Event::Invite { id, code } => json!({
            "kind": "invite",
            "id": id.to_string(),
            "code": code,
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
    fn contacts_arrive_as_full_addresses_with_their_presence() {
        let mut core = ChatCore::new();
        core.receive(&ack("ann")).unwrap();
        events(&mut core);
        let contact = |username: &str, online| cartopolis_core::ContactDto {
            username: username.into(),
            since_ms: 0,
            met: None,
            online,
            agent: false,
        };
        let state = encode_app_frame(&NetMessage::ContactsState {
            contacts: vec![contact("Bob", Some(true)), contact("cy", None)],
            incoming: vec![],
            outgoing: vec![],
            settings: Default::default(),
        })
        .unwrap();
        core.receive(&state).unwrap();
        let ev = events(&mut core);
        assert_eq!(ev[0]["kind"], "contacts");
        assert_eq!(ev[0]["contacts"][0]["address"], "bob@test.example");
        assert_eq!(ev[0]["contacts"][0]["online"], true);
        assert!(
            ev[0]["contacts"][1]["online"].is_null(),
            "hidden, not offline"
        );
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
