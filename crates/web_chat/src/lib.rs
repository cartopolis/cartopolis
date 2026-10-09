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
    decode_app_frame, encode_app_frame, AuthCredential, NetMessage, PROTOCOL_VERSION,
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
                ..
            } => {
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
                    "session_token": session_token,
                }));
                self.username = Some(username);
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
        let username = self
            .username
            .clone()
            .ok_or_else(|| Error("not signed in yet".into()))?;
        let mut engine = match saved {
            Some(bytes) => Engine::load(&bytes).map_err(js_err)?,
            None => Engine::new(&username).map_err(js_err)?,
        };
        if !engine.username().eq_ignore_ascii_case(&username) {
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
    pub fn send_text(&mut self, channel: &str, body: &str, now_ms: f64) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        let engine = self.engine_mut()?;
        let mut out = Vec::new();
        engine
            .send_text(&group, body, now_ms as u64, &mut out)
            .map_err(js_err)?;
        self.dirty = true;
        self.flush(out, Vec::new())
    }

    /// Send a point — a place and the view it was seen from — given as the
    /// ActivityStreams JSON the app's points are (`cartopolis_core::point`).
    pub fn send_point(
        &mut self,
        channel: &str,
        point_json: &str,
        now_ms: f64,
    ) -> Result<(), Error> {
        let group = group_of(channel).ok_or_else(|| Error("not a private chat".into()))?;
        let point = Point::from_activity_json(point_json).map_err(js_err)?;
        let engine = self.engine_mut()?;
        let mut out = Vec::new();
        engine
            .send_point(&group, &point, None, now_ms as u64, &mut out)
            .map_err(js_err)?;
        self.dirty = true;
        self.flush(out, Vec::new())
    }
}

impl ChatCore {
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
            from,
            body,
            point,
            picture,
            sent_at_ms,
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
