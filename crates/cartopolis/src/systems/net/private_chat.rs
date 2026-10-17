//! Private chats on the client: this installation's encryption keys, the
//! conversations they open, and the glue to the chat panel
//! (`docs/notes/encrypted-chat.md`, protocol 25).
//!
//! Two layers. [`Engine`] is plain logic — protocol messages in, protocol
//! messages and [`Event`]s out, no Bevy, no I/O — and lives in `cartopolis_chat`
//! so the web UI can run the same one in the browser; its loopback suite is
//! there too. It is re-exported here, so call sites still name
//! `private_chat::Engine`. [`drive_private_chats`] is the Bevy half: it hands
//! the engine what the network mailbox collected, sends what it answers, turns
//! its events into chat lines, and saves its state.
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

use bevy::prelude::*;
use cartopolis_core::NetMessage;

pub use cartopolis_chat::{
    caption_of, channel_key, group_of, picture_caption, point_caption, readable, Engine, Event,
};

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

/// How the engine's full addresses (`name@server`, protocol 37) are shown and
/// matched in the app: the bare name for somebody on this device's server,
/// which is what the contacts list, the chat panel and the call screen all key
/// on; the full address for anybody else.
fn shown(engine: &Engine, who: &str) -> String {
    cartopolis_core::address::display(who, engine.home()).to_string()
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
        cartopolis_chat::is_private_message(msg)
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
    /// Histories being packed and uploaded for somebody just added, or
    /// fetched and opened after one arrived; finished ones wait here.
    history: std::sync::Arc<std::sync::Mutex<Vec<HistoryDone>>>,
}

/// A history job that finished off the main thread.
enum HistoryDone {
    /// Packed and stored: announce it in `group`.
    Uploaded {
        group: Vec<u8>,
        stored: cartopolis_chat::PictureRef,
        count: u32,
    },
    /// Fetched and opened: put it in front of the chat.
    Opened {
        group: Vec<u8>,
        from: String,
        items: Vec<cartopolis_chat::HistoryItem>,
    },
}

fn finish_history(inbox: &std::sync::Arc<std::sync::Mutex<Vec<HistoryDone>>>, done: HistoryDone) {
    if let Ok(mut v) = inbox.lock() {
        v.push(done);
    }
}

impl PrivateChats {
    /// What a call in this chat needs from its current epoch
    /// (`systems::calls`): the chat's call key and who sends under which
    /// index. `None` before the engine exists or for a chat this device is not in.
    #[must_use]
    pub fn call_epoch(&self, group: &[u8]) -> Option<cartopolis_e2ee::CallEpoch> {
        self.engine
            .as_ref()?
            .call_epoch(group, cartopolis_calls::keys::BASE_KEY_LEN)
            .ok()
    }
    /// The direct chat with `username`, if this device has one — for the call
    /// harness (`calls::drive_call_harness`), which calls a person by name.
    #[cfg(not(target_os = "android"))]
    #[must_use]
    pub fn direct_chat_with(&self, username: &str) -> Option<Vec<u8>> {
        self.engine.as_ref()?.direct_with(username).cloned()
    }

    /// Who else is in a private chat, by username, for its World view
    /// (`contacts::want_chat_whereabouts`). Empty for anything that is not a
    /// private chat this device is in.
    #[must_use]
    pub fn people_in(&self, channel: &str) -> Vec<String> {
        match (self.engine.as_ref(), group_of(channel)) {
            (Some(engine), Some(group)) => engine
                .people_in(&group)
                .iter()
                .map(|p| shown(engine, p))
                .collect(),
            _ => Vec::new(),
        }
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
}

// Everywhere, not only beside the agent tools: a call needs to know whether
// its chat is one-to-one (`calls`, a declined call).
impl PrivateChats {
    /// See [`Engine::describe`].
    #[must_use]
    pub fn describe(&self, group: &[u8]) -> Option<(bool, String)> {
        let engine = self.engine.as_ref()?;
        engine.describe(group).map(|(is_group, label)| {
            // A direct chat's label is the other person's address; a group's
            // is the name it was given.
            let label = if is_group {
                label.to_string()
            } else {
                shown(engine, label)
            };
            (is_group, label)
        })
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
    // A meet-up somebody said they are going to starts its share here, on the
    // same tick that sends shares, so it goes out the moment it is due.
    chat.tick_meetups(now);
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
    mut pictures: ResMut<crate::systems::chat_pictures::ChatPictures>,
    mut communities: ResMut<crate::systems::communities::Communities>,
) {
    let (Some(online), Some(username), Some(server_name)) = (
        online,
        identity.username.clone(),
        identity.server_name.clone(),
    ) else {
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
            // A device is the full address, not the username (protocol 37). A
            // saved state from before that is refused by `Engine::load` above,
            // and this is where its replacement starts.
            None => Engine::new(&cartopolis_core::address::person(&username, &server_name))
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
            chat.open_channel(&channel_key(g), &private_label(&shown(&engine, peer)));
        }
        private.dirty = true;
    }
    let arrived = !inbox.0.is_empty();
    for msg in inbox.0.drain(..) {
        engine.handle(msg, &mut out, &mut events);
    }
    let done: Vec<HistoryDone> = private
        .history
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default();
    for job in done {
        match job {
            HistoryDone::Uploaded {
                group,
                stored,
                count,
            } => {
                if let Err(e) = engine.send_history(&group, &stored, count, now_ms(), &mut out) {
                    tracing::warn!(error = %e, "could not send a history");
                }
                private.dirty = true;
            }
            HistoryDone::Opened { group, from, items } => {
                let lines = items
                    .into_iter()
                    .map(|i| crate::systems::chat::ChatLine {
                        sender: shown(&engine, &i.from),
                        sender_id: 0,
                        body: i.body,
                        distance: None,
                        arrived: f64::NEG_INFINITY,
                        point: i.point.as_deref().and_then(|p| {
                            cartopolis_core::point::Point::from_activity_json(p).ok()
                        }),
                        meeting: i
                            .point
                            .as_deref()
                            .and_then(cartopolis_core::meeting::MeetingObject::from_activity_json),
                        picture: i.picture,
                        at_ms: i.sent_at_ms,
                        seq: 0,
                        log_seq: Some(i.seq),
                        removed: false,
                        request: None,
                        shared_by: None,
                    })
                    .collect();
                chat.insert_history(&channel_key(&group), &shown(&engine, &from), lines);
            }
        }
    }
    for key in std::mem::take(&mut chat.private_forget) {
        if let Some(group) = group_of(&key) {
            engine.forget(&group);
            chat.drop_channel(&key);
            private.dirty = true;
        }
    }
    for op in std::mem::take(&mut communities.ops) {
        use crate::systems::communities::CommunityOp as Op;
        match op {
            Op::Create {
                settings,
                topics,
                people,
            } => engine.create_community(settings, &topics, &people, &mut out),
            Op::Edit(id, s) => engine.edit_community(id, s, &mut out),
            Op::Find(q, order, near) => engine.find_communities(&q, order, near, &mut out),
            Op::CreateInvite(id) => engine.create_invite(id, &mut out),
            Op::LookupInvite(code) => engine.lookup_invite(&code, &mut out),
            Op::JoinInvite(code) => engine.join_invite(&code, &mut out),
            Op::Join(id, m) => engine.join_community(id, &m, &mut out),
            Op::Leave(id) => engine.leave_community(id, &mut out),
            Op::Decide(id, who, accept) => engine.decide_request(id, &who, accept, &mut out),
            Op::Add(id, who) => engine.add_to_community(id, &who, &mut out),
            Op::SetRole(id, who, role) => engine.set_community_role(id, &who, role, &mut out),
            Op::Remove {
                id,
                who,
                topic,
                block,
            } => engine.remove_from_community(id, &who, topic.as_deref(), block, &mut out),
            Op::Delete(id) => engine.delete_community(id, &mut out),
            Op::StartTopic(id, name) => {
                engine.start_topic(id, &name, &mut out, &mut events);
                private.dirty = true;
            }
            Op::EditTopic {
                id,
                group,
                name,
                position,
                archived,
            } => engine.edit_topic(id, &group, &name, position, archived, &mut out),
            Op::ModerateDelete(group, seq) => engine.moderate_delete(&group, seq, &mut out),
            Op::ResolveReport(id, report) => engine.resolve_report(id, report, &mut out),
        }
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
            Ok(request) => {
                let me = shown(&engine, engine.address());
                let my_id = chat.my_id.unwrap_or(0);
                chat.receive(&channel, my_id, me, text, None);
                chat.set_last_request(&channel, request);
            }
            Err(e) => {
                events.push(Event::Notice(format!(
                    "Not sent ({e}). It is back in the message box."
                )));
                chat.unsent.push((channel, text));
            }
        }
        private.dirty = true;
    }
    // A picture goes up first and is announced after: the line that names it
    // must never reach anybody before the thing it names can be fetched.
    for outgoing in std::mem::take(&mut chat.private_pictures) {
        if group_of(&outgoing.channel).is_some() {
            pictures.send(outgoing);
        }
    }
    for (channel, result) in pictures.poll_uploads() {
        let Some(group) = group_of(&channel) else {
            continue;
        };
        let uploaded = match result {
            Ok(u) => u,
            Err(e) => {
                events.push(Event::Notice(format!("picture not sent: {e}")));
                continue;
            }
        };
        match engine.send_picture(
            &group,
            &uploaded.picture,
            &uploaded.caption,
            now_ms(),
            &mut out,
        ) {
            Ok(_) => {
                pictures.insert_local(&uploaded.picture.hash, uploaded.jpeg);
                let me = shown(&engine, engine.address());
                let my_id = chat.my_id.unwrap_or(0);
                chat.receive_picture(
                    &channel,
                    my_id,
                    me,
                    picture_caption(&uploaded.caption),
                    uploaded.picture,
                );
            }
            Err(e) => events.push(Event::Notice(format!("picture not sent: {e}"))),
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
            Ok(_) => {
                let me = shown(&engine, engine.address());
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
    for (channel, meeting) in std::mem::take(&mut chat.private_meetings) {
        let Some(group) = group_of(&channel) else {
            continue;
        };
        match engine.send_meeting(&group, &meeting, now_ms(), &mut out) {
            Ok(request) => {
                let me = shown(&engine, engine.address());
                let my_id = chat.my_id.unwrap_or(0);
                chat.receive_meeting(&channel, my_id, me, meeting.caption(), meeting);
                chat.set_last_request(&channel, request);
            }
            Err(e) => events.push(Event::Notice(format!("not sent: {e}"))),
        }
        private.dirty = true;
    }
    for ev in events {
        match ev {
            Event::Opened { group, peer } => {
                let peer = shown(&engine, &peer);
                let key = channel_key(&group);
                chat.open_channel(&key, &private_label(&peer));
                chat.focus_channel(&key);
                // A first message waiting for this chat to exist
                // (`systems::contacts`): it goes in now, as the request's line.
                if let Some(i) = chat
                    .private_hello
                    .iter()
                    .position(|(who, _)| who.eq_ignore_ascii_case(&peer))
                {
                    let (_, text) = chat.private_hello.remove(i);
                    match engine.send_text(&group, &text, now_ms(), &mut out) {
                        Ok(request) => {
                            let me = shown(&engine, engine.address());
                            let my_id = chat.my_id.unwrap_or(0);
                            chat.receive(&key, my_id, me, text, None);
                            chat.set_last_request(&key, request);
                        }
                        Err(e) => {
                            chat.notice = Some(crate::systems::chat::ChatNotice {
                                text: format!("not sent: {e}"),
                                error: true,
                            });
                        }
                    }
                    private.dirty = true;
                }
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
                ..
            } => {
                let channel = channel_key(&group);
                let filter = private
                    .filter
                    .get_or_insert_with(cartopolis_core::Filter::seeded);
                let (body, _) = filter.mask(&body);
                if let Some(meeting) = object
                    .as_deref()
                    .and_then(cartopolis_core::meeting::MeetingObject::from_activity_json)
                {
                    chat.receive_meeting(&channel, 0, shown(&engine, &from), body, meeting);
                    chat.date_last(&channel, sent_at_ms);
                    chat.set_last_log_seq(&channel, seq);
                    continue;
                }
                if let Some(picture) = picture {
                    chat.receive_picture(&channel, 0, shown(&engine, &from), body, picture);
                    chat.date_last(&channel, sent_at_ms);
                    chat.set_last_log_seq(&channel, seq);
                    continue;
                }
                // A point's own text is masked too: it is shown in the same log.
                let point = point.map(|mut p| {
                    p.text = p.text.map(|t| filter.mask(&t).0);
                    p.name = p.name.map(|n| filter.mask(&n).0);
                    p
                });
                chat.receive_point(&channel, 0, shown(&engine, &from), body, None, point);
                chat.date_last(&channel, sent_at_ms);
                chat.set_last_log_seq(&channel, seq);
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
                    from: shown(&engine, &from),
                    latitude,
                    longitude,
                    accuracy_m,
                    until_ms,
                },
                now_ms(),
            ),
            Event::LocationStopped { group, from } => {
                chat.drop_location(&channel_key(&group), &shown(&engine, &from));
            }
            Event::Named { group, name } => {
                chat.open_channel(&channel_key(&group), &private_label(&name));
            }
            Event::Notice(text) => {
                if communities.directory.is_empty()
                    && matches!(
                        communities.screen,
                        Some(crate::systems::communities::Screen::Invite(_))
                    )
                {
                    communities.invite_refused = true;
                }
                chat.notice = Some(crate::systems::chat::ChatNotice { text, error: true });
            }
            Event::Communities(list) => {
                if let Some(id) = communities.created {
                    if list.iter().any(|c| c.id == id) {
                        communities.created = None;
                        communities.show(id);
                    }
                }
                communities.list = list;
            }
            Event::CommunityCreated(id) => communities.created = Some(id),
            Event::Directory(listings) => communities.directory = listings,
            Event::Invite { id, code } => communities.invite = Some((id, code)),
            Event::Redacted { group, seq, .. } => chat.redact(&channel_key(&group), seq),
            Event::Left {
                group,
                community,
                by,
            } => {
                let key = channel_key(&group);
                let name = communities
                    .list
                    .iter()
                    .find(|c| c.id == community)
                    .map(|c| c.settings.name.clone());
                chat.drop_channel(&key);
                private.dirty = true;
                if !by.is_empty() {
                    chat.notice = Some(crate::systems::chat::ChatNotice {
                        text: match name {
                            Some(n) => {
                                format!("{} took you out of a topic of {n}.", shown(&engine, &by))
                            }
                            None => format!("{} took you out of a topic.", shown(&engine, &by)),
                        },
                        error: false,
                    });
                }
            }
            // Reactions, edits and deletes: the app shows a reply as an
            // ordinary line and keeps no message ids of its own to hang the
            // rest on.
            Event::Reaction { .. } | Event::Edited { .. } | Event::Deleted { .. } => {}
            Event::Sent {
                group,
                request,
                seq,
            } => chat.confirm_sent(&channel_key(&group), request, seq),
            Event::HistoryWanted { group } => {
                let key = channel_key(&group);
                let items: Vec<cartopolis_chat::HistoryItem> = chat
                    .history(&key)
                    .into_iter()
                    .map(|l| cartopolis_chat::HistoryItem {
                        seq: l.log_seq.unwrap_or(0),
                        from: engine.qualify(&l.sender),
                        body: l.body.clone(),
                        sent_at_ms: l.at_ms,
                        parent: None,
                        // The one field carries a point or a meeting's object
                        // (`HistoryItem::point`).
                        point: l
                            .point
                            .as_ref()
                            .and_then(|p| p.to_activity_json(None).ok())
                            .or_else(|| l.meeting.as_ref().map(|m| m.to_activity_json())),
                        picture: l.picture.clone(),
                    })
                    .collect();
                if !items.is_empty() {
                    let inbox = private.history.clone();
                    bevy::tasks::IoTaskPool::get()
                        .spawn(async move {
                            let packed = cartopolis_chat::pack_history(&items);
                            let Ok((sealed, count)) = packed else {
                                return;
                            };
                            let (key, nonce) = (sealed.key, sealed.nonce);
                            match crate::systems::model_assets::api_upload_picture(sealed.bytes)
                                .await
                            {
                                Ok(hash) => finish_history(
                                    &inbox,
                                    HistoryDone::Uploaded {
                                        group,
                                        stored: cartopolis_chat::PictureRef {
                                            hash,
                                            key,
                                            nonce,
                                            width: 0,
                                            height: 0,
                                        },
                                        count,
                                    },
                                ),
                                Err(e) => tracing::warn!(error = %e, "could not store a history"),
                            }
                        })
                        .detach();
                }
            }
            Event::History {
                group,
                from,
                picture,
                ..
            } => {
                let inbox = private.history.clone();
                bevy::tasks::IoTaskPool::get()
                    .spawn(async move {
                        let opened = match crate::systems::model_assets::fetch_picture(
                            &picture.hash,
                        )
                        .await
                        {
                            Ok(bytes) => cartopolis_chat::open_history(&bytes, &picture),
                            Err(e) => Err(e),
                        };
                        match opened {
                            Ok(items) => {
                                finish_history(&inbox, HistoryDone::Opened { group, from, items })
                            }
                            Err(e) => tracing::warn!(error = %e, "could not open a shared history"),
                        }
                    })
                    .detach();
            }
        }
    }
    for msg in out {
        online.send("private chat", msg);
    }
    // Which chat goes with which person, for the contacts screens. Rebuilt
    // rather than patched: it is a handful of entries, and a patch would have
    // to follow every way a chat comes and goes.
    let direct: std::collections::HashMap<String, String> = engine
        .directs()
        .map(|(g, p)| (shown(&engine, p).to_lowercase(), channel_key(g)))
        .collect();
    if chat.direct != direct {
        chat.direct = direct;
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
    fn private_request_ids_do_not_collide_with_the_account_deletion_id() {
        use cartopolis_chat::{is_private_request, REQUEST_BASE};
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
