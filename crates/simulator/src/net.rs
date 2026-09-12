use crate::config::SimConfig;
use crate::state::SimWorld;
use crate::textures;
use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, ChatScope, NetMessage, ProtocolError,
    PROTOCOL_VERSION,
};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use glam::Vec3;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, RwLock};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

/// Chat channel keys and limits (ADR-028).
const GLOBAL_CHANNEL: &str = "geo:";
const LOCAL_CHANNEL: &str = "local";
const MAX_CHAT_LEN: usize = 500;
const CHAT_REPLAY: i64 = 50;
const MIN_LOCAL_INTERVAL: Duration = Duration::from_millis(500);
const MIN_GLOBAL_INTERVAL: Duration = Duration::from_secs(5);
const MIN_ROOM_INTERVAL: Duration = Duration::from_millis(2000);

/// Voice channel key, and limits sized for media rather than typing.
///
/// The text intervals above are seconds apart; continuous speech is a 20 ms
/// frame cadence, so the same limiter shape needs a different order of
/// magnitude — 15 ms admits the expected ~50/s with slack for jitter, and still
/// bounds a client that decides to flood. `MAX_VOICE_BYTES` is generous for an
/// Opus frame (~160 bytes at 64 kbit/s) while staying inside one datagram, so a
/// frame never has to be fragmented when this moves off TCP.
const VOICE_CHANNEL: &str = "voice:local";
const MAX_VOICE_BYTES: usize = 1500;
const MIN_VOICE_INTERVAL: Duration = Duration::from_millis(15);

/// Longest username the moderation messages will act on.
///
/// Same reasoning as `MAX_ROOM_LEN`: `SetBlocked`/`ReportUser` carry a `String`
/// straight off the wire whose only other bound is `MAX_FRAME`, and both end up
/// as a row in SQLite. Comfortably past any account name — `accounts.username`
/// is what these have to match, and nobody signs up with a paragraph.
const MAX_USERNAME_LEN: usize = 64;

/// Turn a client-supplied room name into a channel key, or refuse it.
///
/// `ChatScope::Room` is a `String` straight off the wire and was bounded only by
/// `MAX_FRAME` — 32 MiB. Every use of it allocates again: `format!("room:{room}")`,
/// a clone into the message, and a re-encode per delivery, on a path a client may
/// drive at tens of frames a second. Refusing rather than truncating, because a
/// truncated room name is a *different room* and would silently put someone in a
/// conversation they did not ask for.
fn room_channel(room: &str) -> Option<String> {
    let room = room.trim();
    if room.is_empty() || room.chars().count() > cartopolis_core::MAX_ROOM_LEN {
        return None;
    }
    Some(format!("room:{room}"))
}

/// Milliseconds since the Unix epoch, for chat timestamps.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Recent scrollback for a channel (empty on any lock/query error).
fn chat_history(conn: &Arc<Mutex<rusqlite::Connection>>, channel: &str) -> Vec<crate::db::ChatRow> {
    conn.lock()
        .ok()
        .and_then(|c| crate::db::recent_chat(&c, channel, CHAT_REPLAY).ok())
        .unwrap_or_default()
}

/// Resolve a hello credential to `(principal, username)` (ADR-016). `None` = reject.
///
/// The database is touched for exactly as long as it takes to read one row. The
/// previous version passed the live `MutexGuard` into `verify_credentials`, so the
/// argon2 verify — tens of milliseconds, by design — ran with the SQLite mutex
/// held. That mutex is also taken by `flush_dirty` in the tick loop *while the
/// tick loop holds the world write lock*, so a handful of sockets sending nothing
/// but `ClientHello` could stall the simulation for everyone already playing,
/// before authenticating as anybody. Hashing now happens after the guard is gone,
/// on a blocking thread, behind a concurrency cap (`auth::verify_password_bounded`).
async fn authenticate(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    sessions: &crate::auth::Sessions,
    auth: &AuthCredential,
) -> Result<(uuid::Uuid, String), AuthRefusal> {
    match auth {
        AuthCredential::Token(token) => sessions.resolve(token).ok_or(AuthRefusal::BadCredential),
        AuthCredential::Password { username, password } => {
            let stored = {
                let guard = conn.lock().map_err(|_| AuthRefusal::BadCredential)?;
                crate::auth::lookup_credential(&guard, username)
                    .map_err(|_| AuthRefusal::BadCredential)?
            };
            let stored = stored.ok_or(AuthRefusal::BadCredential)?;
            if !crate::auth::verify_password_bounded(password.clone(), stored.phc).await {
                return Err(AuthRefusal::BadCredential);
            }
            // Only now, with the password proved, may the account's state be
            // reported: telling an unauthenticated caller "confirm your email"
            // would answer "does this username have a pending registration?" for
            // anyone who asked.
            if stored.email_pending {
                return Err(AuthRefusal::EmailUnconfirmed);
            }
            Ok((stored.principal, username.clone()))
        }
    }
}

/// Why a handshake was refused. Two outcomes, deliberately: one of them is the
/// person's own fault and unfixable by them, and the other is a link sitting in
/// their inbox — reporting both as "authentication failed" is how somebody spends
/// an evening retyping a password that was right all along.
enum AuthRefusal {
    BadCredential,
    EmailUnconfirmed,
}

impl AuthRefusal {
    fn message(&self) -> &'static str {
        match self {
            Self::BadCredential => "authentication failed",
            Self::EmailUnconfirmed => {
                "confirm your email address first — check your inbox for the link"
            }
        }
    }
}

/// Also the WebSocket carrier's ceiling (`http::ws_handler`), so neither
/// transport is the more permissive of the two.
pub(crate) const MAX_FRAME: usize = 32 * 1024 * 1024;
/// ADR-012: simple per-connection rate limits (token-bucket style, fixed interval).
/// Intents are *dropped* over this rate, not queued — and they carry absolute
/// state, so a dropped one loses a key edge outright. At the old 50 ms (one full
/// tick) a press or release could sit unapplied for a whole tick on top of the
/// tick quantisation itself, which read as input lag and stutter. 10 ms still
/// caps a flooding client at 100 `world.write()` locks a second.
const MIN_INTENT_INTERVAL: Duration = Duration::from_millis(10);
/// A touch is a deliberate act, so this only has to stop an auto-clicker from
/// making one client's clicks the whole world's script budget.
const MIN_TOUCH_INTERVAL: Duration = Duration::from_millis(100);

/// Sustained rate of script saves from one connection, one per this interval.
///
/// Each save is a SQLite write, a VM teardown and rebuild on the script host, and
/// a `MAX_SOURCE_BYTES` (64 KiB) frame. A refused save is *reported*, never
/// silently dropped like a surplus intent, because an author who is told nothing
/// believes their work was stored.
const MIN_SCRIPT_STORE_INTERVAL: Duration = Duration::from_millis(1000);

/// Saves that may happen back-to-back before the rate above applies.
///
/// A flat "one per second" would be wrong for the thing being limited: saving is
/// a person pressing a button, and people press it twice — the Stop button *is* a
/// second save (an empty source) right after the first. The bucket lets a burst
/// through and only bites on a loop.
const SCRIPT_STORE_BURST: u32 = 5;

/// Whether a prim may be given this model, and why not when it may not.
///
/// Checked when the model is *set* rather than when it is drawn, and refused
/// rather than stored. A hash this server has never held is a typo, a guess, or a
/// model from somebody else's server; storing it makes a prim that no client can
/// ever draw, and — because a model that is merely still downloading looks exactly
/// the same from the client — one that nobody can tell apart from a slow one.
///
/// `None` is always allowed: that is a prim wearing its procedural shape, which
/// is every prim that existed before protocol 22.
fn check_model(conn: &Arc<Mutex<rusqlite::Connection>>, model: Option<&str>) -> Result<(), String> {
    let Some(hash) = model else {
        return Ok(());
    };
    let Ok(c) = conn.lock() else {
        return Err("the model store is unavailable".to_string());
    };
    match crate::library::model_is_available(&c, hash) {
        Ok(true) => Ok(()),
        Ok(false) => Err("this server does not have that model".to_string()),
        // A database error is not a refusal of the model, and saying "no such
        // model" here would send a builder looking for a fault in their upload.
        Err(e) => {
            tracing::warn!("could not check model {hash}: {e:#}");
            Err("could not check that model just now".to_string())
        }
    }
}

/// Tell every client what the world's budget and usage now are.
///
/// Broadcast rather than answered to the actor alone: the budget is shared, so a
/// prim someone else created is headroom this client no longer has, and finding
/// that out from a refusal is finding out too late.
async fn broadcast_capacity(world: &Arc<RwLock<SimWorld>>, tx: &broadcast::Sender<Vec<u8>>) {
    let usage = world.read().await.capacity();
    match encode_app_frame(&capacity_message(&usage)) {
        Ok(bytes) => {
            let _ = tx.send(bytes);
        }
        Err(e) => tracing::error!("capacity encode: {e}"),
    }
}

fn capacity_message(usage: &crate::state::CapacityUsage) -> NetMessage {
    NetMessage::WorldCapacity {
        prims_used: usage.prims_used,
        prims_max: usage.prims_max,
        scripts_used: usage.scripts_used,
        scripts_max: usage.scripts_max,
        script_memory_bytes: crate::scripting::SCRIPT_MEMORY_BYTES as u32,
    }
}

/// One client's byte transport, already framed at message boundaries.
///
/// Two carriers speak the same wire (ADR-008/009): a length-delimited TCP stream,
/// and — since the browser cannot open a TCP socket — one WebSocket **binary**
/// message per frame. Only the framing differs; the frame *body* is the same
/// 8-byte app header plus postcard payload, so `PROTOCOL_VERSION` does not move
/// for a new carrier and a client of either kind is the same client to everything
/// below this line.
///
/// [`handle_connection`] is therefore written against the boundary rather than
/// against the socket. The alternative was a second copy of a 700-line select
/// loop with its own rate limiters, chat replay and avatar teardown — two loops
/// that must stay identical and have no test that says when they stop being so.
pub trait FrameTransport:
    Stream<Item = std::io::Result<bytes::BytesMut>> + Sink<Bytes, Error = std::io::Error> + Unpin + Send
{
}

impl<T> FrameTransport for T where
    T: Stream<Item = std::io::Result<bytes::BytesMut>>
        + Sink<Bytes, Error = std::io::Error>
        + Unpin
        + Send
{
}

/// Wrap an accepted TCP socket in the length-delimited codec (ADR-009): a
/// little-endian `u32` length, then the frame body.
pub fn tcp_frames(stream: TcpStream) -> Framed<TcpStream, LengthDelimitedCodec> {
    Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(MAX_FRAME)
            .little_endian()
            .new_codec(),
    )
}

/// Owns the cleanup for one connection's avatar and chat inbox.
///
/// `handle_connection` spawns the avatar early — the ack that follows carries its
/// id — and then has a long fallible prologue (blocked list, scrollback replay,
/// capacity frames) before the select loop, plus the loop itself. Every `?` on
/// that path used to skip the trailing cleanup, so a client dropping
/// mid-handshake leaked a ghost avatar into every future snapshot and a chat
/// inbox into the hub, permanently. Cleanup on `Drop` covers every exit at once:
/// early return, loop break, and — in a build with unwinding — a panic.
///
/// `Drop` cannot await, so the world write takes `try_write` first (the common
/// case: nothing else holds it at the moment a task unwinds its stack) and falls
/// back to spawning the removal onto the runtime.
struct AvatarGuard {
    world: Arc<RwLock<SimWorld>>,
    chat: Arc<crate::chat::ChatHub>,
    avatar_id: u64,
}

impl Drop for AvatarGuard {
    fn drop(&mut self) {
        // Unregister on a never-registered id is a map remove of nothing.
        self.chat.unregister(self.avatar_id);
        let avatar_id = self.avatar_id;
        if let Ok(mut w) = self.world.try_write() {
            w.remove_avatar(avatar_id);
        } else {
            let world = self.world.clone();
            match tokio::runtime::Handle::try_current() {
                Ok(h) => {
                    h.spawn(async move {
                        world.write().await.remove_avatar(avatar_id);
                    });
                }
                // No runtime left (process teardown) — the world is going away
                // with us, so there is nothing to leak into.
                Err(_) => return,
            }
        }
        tracing::info!(avatar_id, "avatar removed (disconnect)");
    }
}

/// Refuse an edit, and hand back the truth so the client can undo its guess.
///
/// The refusal alone is not enough, and this is the half that is easy to miss.
/// The edit dialog and the gizmo both apply a change **locally** before the
/// server answers — deliberately, for the feel of it — so a client whose edit is
/// refused is left drawing something that never happened: a prim moved where the
/// world does not have it, or (worse) despawned locally on a delete the server
/// declined. Nothing would correct it, because `PrimUpsert` is only broadcast
/// when something *changes*, and a refusal changes nothing.
///
/// So the authoritative state follows the refusal. The client deselects on the
/// 403 and then applies it — in that order, because the client deliberately
/// ignores upserts for the prim it is currently manipulating.
async fn refuse_and_correct<T: FrameTransport>(
    framed: &mut T,
    request_id: u32,
    prim_id: i64,
    truth: Option<cartopolis_core::PrimDto>,
) -> anyhow::Result<()> {
    framed
        .send(Bytes::from(encode_app_frame(&NetMessage::ServerError {
            request_id,
            code: 403,
            message: not_yours(prim_id),
        })?))
        .await?;
    if let Some(prim) = truth {
        framed
            .send(Bytes::from(encode_app_frame(&NetMessage::PrimUpsert {
                prim,
            })?))
            .await?;
    }
    Ok(())
}

/// The refusal a client gets for editing something that is not theirs.
///
/// One sentence, defined once, because it is sent from four places and a rule
/// that is explained differently depending on which message tripped it reads as
/// four different rules.
fn not_yours(prim_id: i64) -> String {
    format!(
        "prim {prim_id} belongs to somebody else. You can change what you built; \
         an operator can change anything."
    )
}

pub async fn handle_connection<T: FrameTransport>(
    mut framed: T,
    world: Arc<RwLock<SimWorld>>,
    config: Arc<SimConfig>,
    mut snap_rx: broadcast::Receiver<Vec<u8>>,
    tx_snap: broadcast::Sender<Vec<u8>>,
    conn: Arc<Mutex<rusqlite::Connection>>,
    sessions: Arc<crate::auth::Sessions>,
    chat: Arc<crate::chat::ChatHub>,
    scripts: crate::scripting::ScriptHandle,
) -> anyhow::Result<()> {
    let first = framed
        .next()
        .await
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("closed before hello"))?;
    let msg = decode_app_frame(&first)?;
    let NetMessage::ClientHello {
        protocol_version,
        auth,
    } = msg
    else {
        return Err(ProtocolError::ExpectedHello(format!("{msg:?}").into()).into());
    };
    if protocol_version != PROTOCOL_VERSION {
        let err = encode_app_frame(&NetMessage::ServerError {
            request_id: 0,
            code: 1,
            message: format!("version {protocol_version} not supported"),
        })?;
        framed.send(Bytes::from(err)).await?;
        return Err(ProtocolError::UnsupportedVersion(protocol_version).into());
    }

    // ADR-016: authenticate before any world traffic. No snapshot or mutating
    // message is served until this succeeds.
    let (principal, username) = match authenticate(&conn, &sessions, &auth).await {
        Ok(p) => p,
        Err(refusal) => {
            let message = refusal.message();
            let err = encode_app_frame(&NetMessage::ServerError {
                request_id: 0,
                code: 401,
                message: message.into(),
            })?;
            framed.send(Bytes::from(err)).await?;
            return Err(anyhow::anyhow!("{message}"));
        }
    };
    let session_token = sessions.issue(principal, &username);

    // Whether this connection may override the per-prim owner check below.
    //
    // Read once at the handshake rather than per edit, which is the same freshness
    // `disabled` already has: `Sessions` is keyed by token with no principal index,
    // so revoking either takes effect on the next `ClientHello` (see `http`'s
    // module note). One rule, not two.
    let is_admin = conn
        .lock()
        .ok()
        .and_then(|c| crate::auth::is_admin(&c, principal).ok())
        .unwrap_or(false);
    let principal_key = principal.to_string();

    let avatar_id = {
        let mut w = world.write().await;
        let id = w.spawn_avatar();
        // Who this avatar is, for anything that has to name it — `vfSensor` hands
        // the name to a script so a prim can greet whoever walked up.
        w.set_avatar_name(id, &username);
        id
    };
    // From here on, every exit — `?`, break, or unwind — retires the avatar.
    let _cleanup = AvatarGuard {
        world: world.clone(),
        chat: chat.clone(),
        avatar_id,
    };
    tracing::info!(%principal, %username, avatar_id, "client authenticated");
    // Display name for chat (the ack moves `username`).
    let display_name = username.clone();
    let ack = encode_app_frame(&NetMessage::ServerHelloAck {
        session_id: uuid::Uuid::new_v4(),
        tick_hz: config.tick_hz,
        your_avatar_id: avatar_id,
        principal_id: principal,
        username,
        session_token,
        osm_tile_url_template: config.osm_tile_url_template.clone(),
        api_base_url: config.resolved_api_base_url(),
    })?;
    framed.send(Bytes::from(ack)).await?;

    // Chat (ADR-028): register a personal delivery inbox, then replay recent
    // Global scrollback so the tab isn't empty on connect.
    //
    // Two inboxes, both bounded, both drained by this loop — see `chat`'s module
    // note. The bound is the load-bearing part: these are written by *other*
    // connections' tasks, and the only drain is a `select!` arm that shares the
    // loop with `framed.send`, so a client whose window has closed stops draining
    // while its neighbours keep speaking.
    // The block list is loaded before the inbox exists and before any scrollback
    // is replayed, which is the only ordering that works: a list fetched *after*
    // the replay would let a blocked person's stored lines through on every
    // single login, which is the one place a block is most obviously broken.
    let blocked = {
        let principal_id = principal.to_string();
        conn.lock()
            .ok()
            .and_then(|c| crate::db::blocked_usernames(&c, &principal_id).ok())
            .unwrap_or_default()
    };
    let blocked_lower: std::collections::HashSet<String> =
        blocked.iter().map(|u| u.to_lowercase()).collect();
    // Built once per connection rather than once per message: a few dozen short
    // strings into a `HashSet`, against a `ChatSend` path that would otherwise
    // rebuild it at typing speed.
    let filter = config.moderation_filter();
    let (mut chat_rx, mut voice_rx) = chat.register(avatar_id, blocked.clone());
    let blocked_frame = encode_app_frame(&NetMessage::BlockedList {
        usernames: blocked.clone(),
    })?;
    framed.send(Bytes::from(blocked_frame)).await?;
    for r in chat_history(&conn, GLOBAL_CHANNEL) {
        if blocked_lower.contains(&r.sender_name.to_lowercase()) {
            continue;
        }
        let frame = encode_app_frame(&NetMessage::ChatMessage {
            channel: GLOBAL_CHANNEL.into(),
            sender_id: r.sender_id as u64,
            sender_name: r.sender_name,
            sent_at_ms: r.sent_at_ms as u64,
            body: r.body,
            distance_m: None,
        })?;
        framed.send(Bytes::from(frame)).await?;
    }

    // The budget, before any authoring is possible: the editor shows headroom
    // rather than discovering it at the moment a create is refused.
    {
        let usage = world.read().await.capacity();
        let frame = encode_app_frame(&capacity_message(&usage))?;
        framed.send(Bytes::from(frame)).await?;
    }

    let mut last_intent = Instant::now()
        .checked_sub(MIN_INTENT_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_touch = Instant::now()
        .checked_sub(MIN_TOUCH_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_local = Instant::now()
        .checked_sub(MIN_LOCAL_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_global = Instant::now()
        .checked_sub(MIN_GLOBAL_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_room = Instant::now()
        .checked_sub(MIN_ROOM_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_voice = Instant::now()
        .checked_sub(MIN_VOICE_INTERVAL)
        .unwrap_or_else(Instant::now);
    // The script-save bucket, carried as "the instant the next save may happen".
    // Adding an interval per save and clamping it to at most `BURST` intervals in
    // the past is a token bucket with no arithmetic to get wrong: the clamp is the
    // refill, the addition is the spend.
    let mut script_store_gate = Instant::now()
        .checked_sub(MIN_SCRIPT_STORE_INTERVAL * SCRIPT_STORE_BURST)
        .unwrap_or_else(Instant::now);

    // Whether this client is being looked at (`NetMessage::SetPresence`).
    //
    // Starts `true`: a client that never sends the message — every build older
    // than protocol 18, and every desktop — behaves exactly as it always did.
    // Only the snapshot arm below reads this; chat, voice, prim updates and
    // everything else are delivered to an idle client unchanged, which is the
    // entire point of holding the connection open.
    let mut presence_active = true;

    let mut outcome: anyhow::Result<()> = Ok(());
    loop {
        // No `biased;`. It polled `framed.next()` first, and the rate limiters in
        // that arm `return Ok(())` without awaiting anything — so a client sending
        // continuously kept the first arm permanently ready and its own snapshot
        // and chat arms were never polled at all. `biased` earns its keep when one
        // arm is a shutdown signal that must win; none of these three is, so the
        // random polling order is the one that keeps a busy client receiving.
        tokio::select! {
            incoming = framed.next() => {
                match incoming {
                    None => break,
                    Some(Err(e)) => {
                        outcome = Err(e.into());
                        break;
                    }
                    Some(Ok(bytes)) => {
                        // Scoped so a `?` in here *leaves the loop* instead of
                        // returning out of `handle_connection`. Every fallible
                        // step below — a truncated postcard body, an encode, a
                        // send — used to skip the teardown at the end of this
                        // function, so one malformed frame from an authenticated
                        // client leaked its avatar into `SimWorld` permanently.
                        // `snapshot` replicates every avatar to every client, so
                        // the leak is visible to everyone as a frozen ghost, at
                        // the tick rate, for the life of the process. The other
                        // two select arms already use exactly this shape.
                        //
                        // The rate-limit skips inside are `return Ok(())` rather
                        // than `continue` for the same reason: this block is the
                        // loop body's continuation, not a loop of its own.
                        let handled: anyhow::Result<()> = async {
                            let msg = decode_app_frame(&bytes)?;
                            match msg {
                            NetMessage::ClientIntent {
                                move_x,
                                move_z,
                                display_yaw,
                                flying,
                                fly_up,
                                fly_down,
                                jump,
                                emote,
                                seated,
                                geo,
                                ..
                            } => {
                                // The rate limit drops surplus intents, but a jump
                                // and an emote are *edges* — dropping one loses the
                                // action outright rather than delaying it by a
                                // frame, so they are never rate limited away.
                                if last_intent.elapsed() < MIN_INTENT_INTERVAL
                                    && !jump
                                    && emote.is_none()
                                {
                                    return Ok(());
                                }
                                last_intent = Instant::now();
                                let mut w = world.write().await;
                                w.apply_intent(
                                    avatar_id,
                                    move_x,
                                    move_z,
                                    display_yaw,
                                    flying,
                                    fly_up,
                                    fly_down,
                                    jump,
                                    emote,
                                    seated,
                                    geo,
                                );
                            }
                            // Deliberately not rate-limited like intents: a
                            // teleport is a deliberate act, not a stream, and
                            // dropping one is not corrected by the next frame.
                            NetMessage::Teleport { position, .. } => {
                                let mut w = world.write().await;
                                w.teleport_avatar(avatar_id, position);
                            }
                            NetMessage::SetPresence { active, .. } => {
                                // Deliberately not rate-limited, unlike the intent
                                // and chat arms above. It is an *edge* — one
                                // message when the app leaves the screen and one
                                // when it comes back — and a dropped one is not
                                // corrected by the next: refusing the "I'm back"
                                // would leave a foregrounded client with no world
                                // state and nothing to make it ask again. A client
                                // that flapped the flag would only be choosing
                                // whether to receive its own snapshots.
                                if presence_active != active {
                                    presence_active = active;
                                    tracing::debug!(avatar_id, active, "presence");
                                }
                            }
                            NetMessage::ClientHello { .. } => {
                                tracing::warn!("duplicate hello ignored");
                            }
                            NetMessage::CreatePrim {
                                request_id, name, position, rotation, scale,
                                color, texture_id, shape, surface, geometry, geo, particles, model,
                            } => {
                                // A model this server does not hold is refused rather than
                                // stored: the alternative is a prim in the street that no
                                // client can ever draw, and nothing to tell that apart from
                                // a download which has not finished yet. Folded into
                                // `created` rather than answered separately, so the refusal
                                // travels the same `ServerError` path every other failed
                                // create already uses.
                                let mut w = world.write().await;
                                // Insert with a server-assigned id, then apply the full initial
                                // authoring state in one update so it is persisted and broadcast.
                                let created = check_model(&conn, model.as_deref()).and_then(|()| {
                                    w.add_prim(position, &shape, geo, particles.as_ref(), model.as_deref()).and_then(|prim| {
                                        w.update_prim(
                                            prim.id, position, rotation, scale, color, texture_id,
                                            &name, surface, geometry, geo, particles.as_ref(), model.as_deref(),
                                        )
                                    })
                                });
                                match created {
                                    Ok(prim) => {
                                        // Whose it is (schema V15). Recorded here
                                        // rather than inside `add_prim` because
                                        // this is the only one of the three create
                                        // paths with a person behind it — the
                                        // admin API and a content pack are the
                                        // server building, and leave it NULL.
                                        // Best-effort: a prim that exists with no
                                        // owner is a prim nobody can export, which
                                        // is better than refusing a build over a
                                        // bookkeeping write.
                                        if let Ok(c) = conn.lock() {
                                            if let Err(e) = crate::db::set_prim_owner(&c, prim.id, &principal_key) {
                                                tracing::warn!(prim_id = prim.id, "recording the owner failed: {e:#}");
                                            }
                                        }
                                        // …and in the world's own map, which is
                                        // what `may_edit` reads. Without this the
                                        // builder could not edit what they had
                                        // just built until the server restarted.
                                        w.set_owner(prim.id, &principal_key);
                                        // Broadcast PrimUpsert to all clients
                                        let broadcast_msg = encode_app_frame(&NetMessage::PrimUpsert { prim })?;
                                        let _ = tx_snap.send(broadcast_msg);
                                        drop(w);
                                        broadcast_capacity(&world, &tx_snap).await;
                                        tracing::debug!(request_id, "prim created and broadcast");
                                    }
                                    Err(e) => {
                                        // Send ServerError back to client
                                        let error_msg = encode_app_frame(&NetMessage::ServerError {
                                            request_id,
                                            code: 400,
                                            message: e.clone(),
                                        })?;
                                        drop(w);
                                        framed.send(Bytes::from(error_msg)).await?;
                                        tracing::warn!(request_id, error = %e, "prim creation failed");
                                    }
                                }
                            }
                            NetMessage::UpdatePrim { request_id, prim_id, position, rotation, scale, color, texture_id, name, surface, geometry, geo, particles, model } => {
                                let mut w = world.write().await;
                                if !w.may_edit(prim_id, &principal_key, is_admin) {
                                    let truth = w.prim(prim_id).cloned();
                                    drop(w);
                                    refuse_and_correct(&mut framed, request_id, prim_id, truth).await?;
                                    return Ok(());
                                }
                                // Same fold as the create above: a model this server does
                                // not hold is a refusal, and it goes back on the path an
                                // update already has for one.
                                let updated = check_model(&conn, model.as_deref()).and_then(|()| {
                                    w.update_prim(prim_id, position, rotation, scale, color, texture_id, &name, surface, geometry, geo, particles.as_ref(), model.as_deref())
                                });
                                match updated {
                                    Ok(prim) => {
                                        // Broadcast PrimUpsert to all clients
                                        let broadcast_msg = encode_app_frame(&NetMessage::PrimUpsert { prim })?;
                                        let _ = tx_snap.send(broadcast_msg);
                                        tracing::debug!(request_id, prim_id, "prim updated and broadcast");
                                    }
                                    Err(e) => {
                                        // Send ServerError back to client
                                        let error_msg = encode_app_frame(&NetMessage::ServerError {
                                            request_id,
                                            code: 404,
                                            message: e.clone(),
                                        })?;
                                        drop(w);
                                        framed.send(Bytes::from(error_msg)).await?;
                                        tracing::warn!(request_id, prim_id, error = %e, "prim update failed");
                                    }
                                }
                            }
                            NetMessage::DeletePrim { request_id, prim_id } => {
                                let mut w = world.write().await;
                                if !w.may_edit(prim_id, &principal_key, is_admin) {
                                    let truth = w.prim(prim_id).cloned();
                                    drop(w);
                                    refuse_and_correct(&mut framed, request_id, prim_id, truth).await?;
                                    return Ok(());
                                }
                                match w.remove_prim(prim_id) {
                                    Ok(deleted) => {
                                        if deleted {
                                            // The prim's script goes with it — the row
                                            // is deleted by `remove_prim`, but the live
                                            // VM is the host's and has to be told.
                                            scripts.unload(prim_id);
                                            // Broadcast PrimRemoved to all clients
                                            let broadcast_msg = encode_app_frame(&NetMessage::PrimRemoved { id: prim_id })?;
                                            let _ = tx_snap.send(broadcast_msg);
                                            drop(w);
                                            broadcast_capacity(&world, &tx_snap).await;
                                            tracing::debug!(request_id, prim_id, "prim deleted and broadcast");
                                        } else {
                                            // Send ServerError back to client (not found)
                                            let error_msg = encode_app_frame(&NetMessage::ServerError {
                                                request_id,
                                                code: 404,
                                                message: format!("prim {} not found", prim_id),
                                            })?;
                                            drop(w);
                                            framed.send(Bytes::from(error_msg)).await?;
                                            tracing::warn!(request_id, prim_id, "prim not found for deletion");
                                        }
                                    }
                                    Err(e) => {
                                        // Send ServerError back to client
                                        let error_msg = encode_app_frame(&NetMessage::ServerError {
                                            request_id,
                                            code: 500,
                                            message: e.clone(),
                                        })?;
                                        drop(w);
                                        framed.send(Bytes::from(error_msg)).await?;
                                        tracing::warn!(request_id, prim_id, error = %e, "prim deletion failed");
                                    }
                                }
                            }
                            NetMessage::TextureCatalogRequest => {
                                let entries = textures::list_textures(&config.texture_dir);
                                let msg = encode_app_frame(&NetMessage::TextureCatalog { textures: entries })?;
                                framed.send(Bytes::from(msg)).await?;
                            }
                            NetMessage::TextureRequest { request_id, texture_id } => {
                                match textures::load_texture(&config.texture_dir, &texture_id) {
                                    Some(png_bytes) => {
                                        let msg = encode_app_frame(&NetMessage::TextureData { request_id, texture_id, png_bytes })?;
                                        framed.send(Bytes::from(msg)).await?;
                                    }
                                    None => {
                                        let msg = encode_app_frame(&NetMessage::TextureNotFound { request_id, texture_id })?;
                                        framed.send(Bytes::from(msg)).await?;
                                    }
                                }
                            }
                            NetMessage::ChatSend { scope, body, .. } => {
                                let body: String = body.trim().chars().take(MAX_CHAT_LEN).collect();
                                if body.is_empty() {
                                    return Ok(());
                                }
                                // Masked once, here, before the message is stored,
                                // echoed or fanned out — so every copy that exists
                                // anywhere is the masked one, and a client cannot
                                // opt out of the filter by not applying it.
                                let (body, masked) = filter.mask(&body);
                                if masked > 0 {
                                    tracing::debug!(%display_name, masked, "chat line filtered");
                                }
                                let now_ms = now_millis();
                                match scope {
                                    ChatScope::Global => {
                                        if last_global.elapsed() < MIN_GLOBAL_INTERVAL {
                                            return Ok(());
                                        }
                                        last_global = Instant::now();
                                        if let Ok(c) = conn.lock() {
                                            // Delivered live either way, but a store that failed means the
                                            // scrollback replay will silently miss this line — say so.
                                            if let Err(e) = crate::db::insert_chat(&c, GLOBAL_CHANNEL, avatar_id as i64, &display_name, now_ms as i64, &body) {
                                                tracing::warn!(avatar_id, "storing global chat failed: {e:#}");
                                            }
                                        }
                                        let frame = encode_app_frame(&NetMessage::ChatMessage {
                                            channel: GLOBAL_CHANNEL.into(), sender_id: avatar_id,
                                            sender_name: display_name.clone(), sent_at_ms: now_ms,
                                            body, distance_m: None,
                                        })?;
                                        chat.deliver_all(&display_name, &frame);
                                    }
                                    ChatScope::Room(room) => {
                                        // A member first, and a valid name first of
                                        // all. Membership was only ever consulted
                                        // for *recipients*, so any client could
                                        // address — and write history into — a room
                                        // it had never joined, just by naming it.
                                        let Some(channel) = room_channel(&room) else {
                                            return Ok(());
                                        };
                                        if !chat.in_room(avatar_id, &channel) {
                                            tracing::debug!(avatar_id, "room message from a non-member");
                                            return Ok(());
                                        }
                                        if last_room.elapsed() < MIN_ROOM_INTERVAL {
                                            return Ok(());
                                        }
                                        last_room = Instant::now();
                                        if let Ok(c) = conn.lock() {
                                            if let Err(e) = crate::db::insert_chat(&c, &channel, avatar_id as i64, &display_name, now_ms as i64, &body) {
                                                tracing::warn!(avatar_id, channel = %channel, "storing room chat failed: {e:#}");
                                            }
                                        }
                                        let frame = encode_app_frame(&NetMessage::ChatMessage {
                                            channel: channel.clone(), sender_id: avatar_id,
                                            sender_name: display_name.clone(), sent_at_ms: now_ms,
                                            body, distance_m: None,
                                        })?;
                                        chat.deliver_room(&channel, &display_name, crate::chat::Lane::Text, &frame);
                                    }
                                    ChatScope::Local { shout } => {
                                        if last_local.elapsed() < MIN_LOCAL_INTERVAL {
                                            return Ok(());
                                        }
                                        last_local = Instant::now();
                                        let radius = if shout { crate::chat::SHOUT_RADIUS_M } else { crate::chat::SAY_RADIUS_M };
                                        // Sender + all candidate recipient positions in one read.
                                        let (sender_pos, positions) = {
                                            let w = world.read().await;
                                            let sp = w.avatar_position(avatar_id);
                                            let ps: Vec<(u64, _)> = chat.connected_ids().into_iter()
                                                .filter_map(|id| w.avatar_position(id).map(|p| (id, p)))
                                                .collect();
                                            (sp, ps)
                                        };
                                        // Local is ephemeral — delivered by distance, never stored.
                                        if let Some(sp) = sender_pos {
                                            for (id, d) in crate::chat::within_radius(sp, &positions, radius) {
                                                let frame = encode_app_frame(&NetMessage::ChatMessage {
                                                    channel: LOCAL_CHANNEL.into(), sender_id: avatar_id,
                                                    sender_name: display_name.clone(), sent_at_ms: now_ms,
                                                    body: body.clone(), distance_m: Some(d),
                                                })?;
                                                chat.deliver_to(id, &display_name, crate::chat::Lane::Text, frame);
                                            }
                                        }
                                    }
                                }
                            }
                            NetMessage::ChatSubscribe { room, join } => {
                                let Some(channel) = room_channel(&room) else {
                                    return Ok(());
                                };
                                chat.set_room(avatar_id, &channel, join);
                                if join {
                                    for r in chat_history(&conn, &channel) {
                                        let frame = encode_app_frame(&NetMessage::ChatMessage {
                                            channel: channel.clone(), sender_id: r.sender_id as u64,
                                            sender_name: r.sender_name, sent_at_ms: r.sent_at_ms as u64,
                                            body: r.body, distance_m: None,
                                        })?;
                                        framed.send(Bytes::from(frame)).await?;
                                    }
                                }
                            }
                            NetMessage::GetPrimScript { request_id, prim_id } => {
                                let source = conn.lock().ok()
                                    .and_then(|c| crate::db::get_prim_script(&c, prim_id).ok().flatten())
                                    .unwrap_or_default();
                                let running = { world.read().await.is_scripted(prim_id) };
                                let msg = encode_app_frame(&NetMessage::PrimScript { prim_id, source, running })?;
                                framed.send(Bytes::from(msg)).await?;
                                tracing::debug!(request_id, prim_id, "script sent to editor");
                            }
                            NetMessage::SetPrimScript { request_id, prim_id, source } => {
                                // Trim first: an editor that saves an untouched
                                // buffer of whitespace means "no script", not "a
                                // script that does nothing", and the two differ in
                                // whether they draw against the world's budget.
                                let source = source.trim().to_string();
                                let within_rate = {
                                    let now = Instant::now();
                                    let floor = now
                                        .checked_sub(MIN_SCRIPT_STORE_INTERVAL * SCRIPT_STORE_BURST)
                                        .unwrap_or(now);
                                    if script_store_gate < floor {
                                        script_store_gate = floor;
                                    }
                                    let ok = script_store_gate <= now;
                                    if ok {
                                        script_store_gate += MIN_SCRIPT_STORE_INTERVAL;
                                    }
                                    ok
                                };
                                let outcome = if !within_rate {
                                    Err("saving too fast — try again in a moment".to_string())
                                } else if source.len() > crate::scripting::MAX_SOURCE_BYTES {
                                    Err(format!(
                                        "script is {} bytes; the limit is {}",
                                        source.len(),
                                        crate::scripting::MAX_SOURCE_BYTES
                                    ))
                                } else {
                                    let mut w = world.write().await;
                                    if !w.has_prim(prim_id) {
                                        Err(format!("prim {prim_id} not found"))
                                    } else if !w.may_edit(prim_id, &principal_key, is_admin) {
                                        // Scripting somebody else's object is the
                                        // most complete way to take it over —
                                        // a script can move it, rename it, set it
                                        // alight and delete it — so this is the
                                        // same gate as an edit, not a lesser one.
                                        Err(not_yours(prim_id))
                                    } else if source.is_empty() {
                                        // Removing a script: unload the VM, drop the
                                        // row, release the budget.
                                        let stored = conn.lock().ok()
                                            .map(|c| crate::db::delete_prim_script(&c, prim_id).unwrap_or(false));
                                        match stored {
                                            Some(_) => {
                                                w.set_scripted(prim_id, false);
                                                scripts.unload(prim_id);
                                                Ok(())
                                            }
                                            None => Err("database is unavailable".to_string()),
                                        }
                                    } else {
                                        match w.can_add_script(prim_id) {
                                            Err(e) => Err(e),
                                            Ok(()) => {
                                                let stored = conn.lock().ok().map(|c| {
                                                    crate::db::set_prim_script(&c, prim_id, &source)
                                                });
                                                match stored {
                                                    Some(Ok(())) => {
                                                        w.set_scripted(prim_id, true);
                                                        // Compile errors come back
                                                        // asynchronously as
                                                        // `ScriptOutput`; the store
                                                        // itself has already succeeded.
                                                        scripts.load(prim_id, source.clone());
                                                        Ok(())
                                                    }
                                                    Some(Err(e)) => Err(format!("could not store script: {e}")),
                                                    None => Err("database is unavailable".to_string()),
                                                }
                                            }
                                        }
                                    }
                                };
                                match outcome {
                                    Ok(()) => {
                                        // Acknowledged to the author, not broadcast.
                                        //
                                        // This used to go to `tx_snap` — the
                                        // whole-world channel — so saving a script
                                        // sent its **source** to every connected
                                        // client. That is a disclosure (a script is
                                        // the one part of a prim its author may not
                                        // want read) and a 64 KiB frame per save
                                        // against a 256-slot broadcast, which a loop
                                        // of saves turns into everybody else's
                                        // snapshots being dropped as `Lagged`.
                                        //
                                        // What it bought was a second editor open on
                                        // the same prim following along. That editor
                                        // can ask: `GetPrimScript` is one message and
                                        // it is the same message it opened with.
                                        let running = !source.is_empty();
                                        let frame = encode_app_frame(&NetMessage::PrimScript { prim_id, source, running })?;
                                        framed.send(Bytes::from(frame)).await?;
                                        broadcast_capacity(&world, &tx_snap).await;
                                    }
                                    Err(e) => {
                                        // A refusal is console text, not a modal:
                                        // the editor is where the author is looking,
                                        // and it is the only place with room to say
                                        // which limit was hit.
                                        let frame = encode_app_frame(&NetMessage::ScriptOutput {
                                            prim_id,
                                            level: cartopolis_core::ScriptLevel::Error,
                                            text: e.clone(),
                                            at_ms: now_millis(),
                                        })?;
                                        framed.send(Bytes::from(frame)).await?;
                                        tracing::warn!(request_id, prim_id, error = %e, "script store refused");
                                    }
                                }
                            }
                            NetMessage::TouchPrim { request_id, prim_id } => {
                                if last_touch.elapsed() < MIN_TOUCH_INTERVAL {
                                    return Ok(());
                                }
                                last_touch = Instant::now();
                                // Only a prim that has a script gets an event; a
                                // touch on plain geometry is not an error, it is
                                // simply nothing happening.
                                if world.read().await.is_scripted(prim_id) {
                                    scripts.event(prim_id, crate::scripting::ScriptEvent::Touch {
                                        avatar_id,
                                        avatar_name: display_name.clone(),
                                    });
                                    tracing::debug!(request_id, prim_id, avatar_id, "prim touched");
                                }
                            }
                            NetMessage::VoiceFrame { scope, seq, payload } => {
                                if payload.is_empty() || payload.len() > MAX_VOICE_BYTES {
                                    return Ok(());
                                }
                                if last_voice.elapsed() < MIN_VOICE_INTERVAL {
                                    return Ok(());
                                }
                                last_voice = Instant::now();
                                // Voice is never stored: it is ephemeral in every
                                // scope, not just Local, so no `insert_chat` here.
                                match scope {
                                    ChatScope::Local { shout } => {
                                        let radius = if shout { crate::chat::SHOUT_RADIUS_M } else { crate::chat::SAY_RADIUS_M };
                                        let (sender_pos, positions) = {
                                            let w = world.read().await;
                                            let sp = w.avatar_position(avatar_id);
                                            let ps: Vec<(u64, _)> = chat.connected_ids().into_iter()
                                                .filter(|id| *id != avatar_id)
                                                .filter_map(|id| w.avatar_position(id).map(|p| (id, p)))
                                                .collect();
                                            (sp, ps)
                                        };
                                        if let Some(sp) = sender_pos {
                                            for (id, d) in crate::chat::within_radius(sp, &positions, radius) {
                                                let frame = encode_app_frame(&NetMessage::VoiceData {
                                                    channel: VOICE_CHANNEL.into(), sender_id: avatar_id,
                                                    seq, payload: payload.clone(), distance_m: Some(d),
                                                })?;
                                                chat.deliver_to(id, &display_name, crate::chat::Lane::Voice, frame);
                                            }
                                        }
                                    }
                                    ChatScope::Room(room) => {
                                        // The same membership rule as room text, and
                                        // it matters more here: this is the one path
                                        // that runs at ~66 frames a second, so an
                                        // unchecked room name was a way to shout into
                                        // any conversation on the server at the cost
                                        // of naming it — and to make the server
                                        // allocate three copies of a 32 MB string per
                                        // frame while doing it.
                                        let Some(channel) = room_channel(&room) else {
                                            return Ok(());
                                        };
                                        if !chat.in_room(avatar_id, &channel) {
                                            tracing::debug!(avatar_id, "room voice from a non-member");
                                            return Ok(());
                                        }
                                        let frame = encode_app_frame(&NetMessage::VoiceData {
                                            channel: channel.clone(), sender_id: avatar_id,
                                            seq, payload, distance_m: None,
                                        })?;
                                        chat.deliver_room_except(&channel, avatar_id, &display_name, crate::chat::Lane::Voice, &frame);
                                    }
                                    // Global voice is deliberately not carried: it is
                                    // an unbounded fan-out and the loudest abuse
                                    // surface chat has, and ADR-028 already suggests
                                    // launching without the widest text tiers. Enable
                                    // it here if that call is revisited.
                                    ChatScope::Global => {
                                        tracing::debug!("dropped global voice frame");
                                    }
                                }
                            }
                            // No `request_id`: the answer is the whole
                            // `BlockedList`, not an ack, so there is nothing to
                            // correlate.
                            NetMessage::SetBlocked { request_id: _, username: subject, blocked: on } => {
                                let subject = subject.trim().to_string();
                                // Blocking yourself would take your own words out
                                // of every channel you speak in, and reads as the
                                // chat being broken. Refused, silently: nobody
                                // means it.
                                if subject.is_empty()
                                    || subject.eq_ignore_ascii_case(&display_name)
                                    || subject.chars().count() > MAX_USERNAME_LEN
                                {
                                    return Ok(());
                                }
                                let principal_id = principal.to_string();
                                let list = {
                                    let Ok(c) = conn.lock() else { return Ok(()) };
                                    if let Err(e) = crate::db::set_block(&c, &principal_id, &subject, on) {
                                        tracing::warn!(error = %e, "block write failed");
                                        return Ok(());
                                    }
                                    crate::db::blocked_usernames(&c, &principal_id).unwrap_or_default()
                                };
                                // The live connection and the row move together;
                                // a block that only lands at the next login is not
                                // a block anyone would call working.
                                chat.set_blocked(avatar_id, &subject, on);
                                tracing::info!(%display_name, %subject, blocked = on, "block updated");
                                let frame = encode_app_frame(&NetMessage::BlockedList { usernames: list })?;
                                framed.send(Bytes::from(frame)).await?;
                            }
                            NetMessage::ReportUser { request_id, username: subject, reason, note, message, asset } => {
                                let subject = subject.trim().to_string();
                                let accepted = !subject.is_empty()
                                    && !subject.eq_ignore_ascii_case(&display_name)
                                    && subject.chars().count() <= MAX_USERNAME_LEN;
                                if accepted {
                                    let note: String = note.trim().chars().take(cartopolis_core::MAX_REPORT_NOTE_LEN).collect();
                                    let message: Option<String> = message.map(|m| {
                                        m.chars().take(cartopolis_core::MAX_REPORT_MESSAGE_LEN).collect()
                                    });
                                    let principal_id = principal.to_string();
                                    // A hash that is not one is dropped rather than
                                    // stored: the report still stands against the
                                    // person, and a garbage value in this column
                                    // would send an operator looking for a model
                                    // that never existed.
                                    let asset = asset.filter(|a| crate::assets::is_hash(a));
                                    if let Ok(c) = conn.lock() {
                                        match crate::db::insert_report(
                                            &c, &principal_id, &display_name, &subject,
                                            reason.as_str(), &note, message.as_deref(), asset.as_deref(),
                                        ) {
                                            // At `warn`, and with the subject in it,
                                            // because on a small deployment the log
                                            // *is* the moderation queue until
                                            // somebody opens the admin API.
                                            Ok(id) => tracing::warn!(
                                                report_id = id, reporter = %display_name, %subject,
                                                reason = reason.as_str(), "user reported"
                                            ),
                                            Err(e) => tracing::error!(error = %e, "report write failed"),
                                        }
                                    }
                                }
                                let frame = encode_app_frame(&NetMessage::ReportAck { request_id, accepted })?;
                                framed.send(Bytes::from(frame)).await?;
                            }
                            NetMessage::DeleteAccount { request_id, password } => {
                                // The password is re-checked even though this
                                // connection is authenticated: a session token can
                                // be a remembered one on a device somebody else is
                                // holding, and this is the one request that cannot
                                // be undone. Hashing happens off the SQLite mutex,
                                // for the reason `authenticate` documents.
                                let phc = {
                                    let Ok(c) = conn.lock() else { return Ok(()) };
                                    crate::auth::lookup_account(&c, &display_name).ok().flatten()
                                };
                                let ok = match phc {
                                    Some((_, phc)) => crate::auth::verify_password_bounded(password, phc).await,
                                    None => false,
                                };
                                if !ok {
                                    let frame = encode_app_frame(&NetMessage::ServerError {
                                        request_id, code: 401,
                                        message: "that password is not right".into(),
                                    })?;
                                    framed.send(Bytes::from(frame)).await?;
                                    return Ok(());
                                }
                                let outcome = {
                                    let Ok(c) = conn.lock() else { return Ok(()) };
                                    crate::auth::delete_own_account(&c, principal, &display_name)
                                };
                                match outcome {
                                    Ok(crate::auth::SelfDelete::Deleted) => {
                                        let revoked = sessions.revoke_principal(principal);
                                        tracing::warn!(%display_name, revoked, "account deleted by its owner");
                                        let frame = encode_app_frame(&NetMessage::AccountDeleted)?;
                                        framed.send(Bytes::from(frame)).await?;
                                        // The account is gone, so this connection is
                                        // authenticated as nobody. Ending it here is
                                        // what makes that true of the socket too.
                                        return Err(anyhow::anyhow!("account deleted"));
                                    }
                                    Ok(crate::auth::SelfDelete::LastAdmin) => {
                                        let frame = encode_app_frame(&NetMessage::ServerError {
                                            request_id, code: 409,
                                            message: "this is the last admin account; grant admin to someone else first".into(),
                                        })?;
                                        framed.send(Bytes::from(frame)).await?;
                                    }
                                    Ok(crate::auth::SelfDelete::Unknown) | Err(_) => {
                                        let frame = encode_app_frame(&NetMessage::ServerError {
                                            request_id, code: 500,
                                            message: "could not delete the account".into(),
                                        })?;
                                        framed.send(Bytes::from(frame)).await?;
                                    }
                                }
                            }
                            NetMessage::PrimRemoved { .. }
                            | NetMessage::WorldSnapshot { .. }
                            | NetMessage::ServerHelloAck { .. }
                            | NetMessage::ServerError { .. }
                            | NetMessage::PrimUpsert { .. }
                            | NetMessage::TextureCatalog { .. }
                            | NetMessage::TextureData { .. }
                            | NetMessage::TextureNotFound { .. }
                            | NetMessage::ChatMessage { .. }
                            | NetMessage::PrimScript { .. }
                            | NetMessage::ScriptOutput { .. }
                            | NetMessage::WorldCapacity { .. }
                            | NetMessage::VoiceData { .. }
                            | NetMessage::BlockedList { .. }
                            | NetMessage::ReportAck { .. }
                            | NetMessage::AccountDeleted => {
                                tracing::debug!("ignored message from client");
                            }
                            }
                            Ok(())
                        }
                        .await;
                        if let Err(e) = handled {
                            outcome = Err(e);
                            break;
                        }
                    }
                }
            }
            snap = snap_rx.recv() => {
                match snap {
                    Ok(bytes) => {
                        // This channel carries prim upserts, removals and capacity
                        // updates as well as snapshots, so the filter is per-kind
                        // and not per-channel: an idle client still needs to be
                        // told a prim changed, it just does not need thirty full
                        // world states a second it cannot draw. `frame_kind` reads
                        // the header rather than decoding the payload — decoding a
                        // whole snapshot to discard it would cost more than
                        // sending it.
                        if !presence_active
                            && cartopolis_core::protocol::frame_kind(&bytes)
                                == Some(cartopolis_core::protocol::MessageKind::WorldSnapshot)
                        {
                            continue;
                        }
                        if let Err(e) = framed.send(Bytes::from(bytes)).await {
                            outcome = Err(e.into());
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            chat_frame = chat_rx.recv() => {
                match chat_frame {
                    Some(frame) => {
                        if let Err(e) = framed.send(Bytes::from(frame)).await {
                            outcome = Err(e.into());
                            break;
                        }
                    }
                    // The hub holds the only sender and drops it in `unregister`,
                    // which this loop calls on its way out — so a closed inbox
                    // means the connection is already finished.
                    None => break,
                }
            }
            voice_frame = voice_rx.recv() => {
                match voice_frame {
                    Some(frame) => {
                        if let Err(e) = framed.send(Bytes::from(frame)).await {
                            outcome = Err(e.into());
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    // Cleanup (avatar + chat inbox) is `_cleanup`'s Drop, which also covers the
    // early returns above the loop.
    outcome
}

/// How often script writes are pushed to SQLite, in ticks. A script that moves a
/// prim does so every tick; persisting each one would be 20 writes a second per
/// script for state that is about to change again. The in-memory value is what
/// clients see, so the only thing this cadence costs is how much motion a crash
/// rewinds.
const SCRIPT_FLUSH_TICKS: u64 = 40;

/// Periodically steps simulation and broadcasts postcard-encoded [`NetMessage::WorldSnapshot`] in app frames.
pub async fn tick_loop(
    world: Arc<RwLock<SimWorld>>,
    config: Arc<SimConfig>,
    tx: broadcast::Sender<Vec<u8>>,
    // Owned, not shared: the host's command receiver is `!Sync`, so an `Arc` of it
    // could not cross a `tokio::spawn`. This is the one place that drains it, which
    // is the same reason [`ScriptHost`] and [`ScriptHandle`] are separate types.
    scripts: crate::scripting::ScriptHost,
    chat: Arc<crate::chat::ChatHub>,
) {
    let period = std::time::Duration::from_secs_f32((1.0 / config.tick_hz).max(0.001));
    let mut interval = tokio::time::interval(period);
    // `Burst` (the default) fires missed ticks back-to-back to catch up, which
    // lands two or three steps' worth of motion in one instant and then leaves a
    // gap — an uneven snapshot cadence the client's prediction can only read as a
    // jump. Delay the schedule instead and let the measured `dt` below account
    // for the lost time.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // A stalled runtime shouldn't teleport every avatar on the next tick.
    let max_step = period.as_secs_f32() * 5.0;
    let mut last_step = Instant::now();
    let mut tick: u64 = 0;
    loop {
        interval.tick().await;
        tick += 1;
        // Step by *measured* elapsed time, not the nominal period. Solo mode runs
        // this loop in-process alongside the Bevy render loop, so ticks routinely
        // land late; advancing a fixed 50 ms per tick let simulated time fall
        // behind wall-clock time, and the client — predicting against a real
        // clock — then ran permanently ahead of the authority it was reconciling
        // toward. The gap is proportional to speed, so it was invisible at an
        // 8 m/s walk and obvious at the 32 m/s fly.
        let now = Instant::now();
        let dt = (now - last_step).as_secs_f32().min(max_step);
        last_step = now;
        let mut w = world.write().await;
        w.step(dt);

        // Scripts are applied at the tick barrier: everything the host produced
        // since the last tick lands in one batch, before the snapshot that
        // carries it, so a client never sees half a script's frame of work.
        let commands = scripts.drain();
        let mut script_frames: Vec<Vec<u8>> = Vec::new();
        let mut died: Vec<i64> = Vec::new();
        let mut changed: Vec<i64> = Vec::new();
        for cmd in &commands {
            // `vfDie` is handled before the field writes, and its prim is skipped
            // by everything after: a script's last act is often a move *and* a
            // death in the same tick, and broadcasting an update for a prim that
            // is about to be removed leaves the client holding a resurrected copy.
            if let crate::scripting::ScriptCommand::Die { prim_id } = cmd {
                match w.remove_prim(*prim_id) {
                    Ok(true) => {
                        died.push(*prim_id);
                        scripts.unload(*prim_id);
                        match encode_app_frame(&NetMessage::PrimRemoved { id: *prim_id }) {
                            Ok(bytes) => script_frames.push(bytes),
                            Err(e) => tracing::error!("script prim removed encode: {e}"),
                        }
                    }
                    Ok(false) => {}
                    Err(e) => tracing::warn!(prim_id, "vfDie failed: {e:#}"),
                }
                continue;
            }
            if died.contains(&cmd.prim_id()) {
                continue;
            }
            if w.apply_script_command(cmd).is_some() {
                changed.push(cmd.prim_id());
            }
        }
        // One update per prim per tick, not one per field written. A handler that
        // sets a name, a colour and a rotation is describing *one* state, and
        // sending three full `PrimDto`s makes observers render two intermediates
        // that never existed as a resting state — as well as tripling the traffic
        // for a script that touches several fields, which is the normal case.
        changed.sort_unstable();
        changed.dedup();
        for prim_id in changed {
            let Some(prim) = w.prim(prim_id).cloned() else {
                continue;
            };
            match encode_app_frame(&NetMessage::PrimUpsert { prim }) {
                Ok(bytes) => script_frames.push(bytes),
                Err(e) => tracing::error!("script prim upsert encode: {e}"),
            }
        }
        if tick % SCRIPT_FLUSH_TICKS == 0 {
            w.flush_dirty();
        }
        let snap = w.snapshot(tick);
        let views = w.script_views();
        // Chat and console output need the world only for lookups; take what they
        // need here and release the write lock before any encoding or delivery.
        let speech: Vec<(String, Vec3, String, f32)> = commands
            .iter()
            .filter_map(|cmd| match cmd {
                crate::scripting::ScriptCommand::Say {
                    prim_id,
                    text,
                    radius,
                } => w
                    .prim(*prim_id)
                    .map(|p| (p.name.clone(), p.position, text.clone(), *radius)),
                _ => None,
            })
            .collect();
        let listeners: Vec<(u64, Vec3)> = if speech.is_empty() {
            Vec::new()
        } else {
            chat.connected_ids()
                .into_iter()
                .filter_map(|id| w.avatar_position(id).map(|p| (id, p)))
                .collect()
        };
        drop(w);

        for bytes in script_frames {
            let _ = tx.send(bytes);
        }
        // A prim that deleted itself gives its budget back, and every client's
        // headroom display is stale until it is told.
        if !died.is_empty() {
            broadcast_capacity(&world, &tx).await;
        }
        // A prim's speech is local chat like anyone else's: same channel, same
        // radius, same "no history" rule. `sender_id` is 0 — a prim has no avatar
        // id, and the client tags own-lines by that id, so a real one would make
        // whoever shares the number appear to be talking.
        let now_ms = now_millis();
        for (name, pos, text, radius) in speech {
            for (id, listener) in &listeners {
                let d = (*listener - pos).length();
                if d > radius {
                    continue;
                }
                match encode_app_frame(&NetMessage::ChatMessage {
                    channel: LOCAL_CHANNEL.into(),
                    sender_id: 0,
                    sender_name: name.clone(),
                    sent_at_ms: now_ms,
                    body: text.clone(),
                    distance_m: Some(d),
                }) {
                    Ok(frame) => {
                        chat.deliver_to(*id, name.as_str(), crate::chat::Lane::Text, frame)
                    }
                    Err(e) => tracing::error!("script say encode: {e}"),
                }
            }
        }
        // Console lines go to every connection rather than to a registry of open
        // editors: they are rate-capped at the host (`MAX_COMMANDS_PER_TICK`), a
        // client with no editor open drops them on the floor, and a per-editor
        // subscription would be a second thing to keep in step with disconnects.
        for cmd in &commands {
            if let crate::scripting::ScriptCommand::Output {
                prim_id,
                level,
                text,
            } = cmd
            {
                match encode_app_frame(&NetMessage::ScriptOutput {
                    prim_id: *prim_id,
                    level: *level,
                    text: text.clone(),
                    at_ms: now_ms,
                }) {
                    Ok(frame) => {
                        let _ = tx.send(frame);
                    }
                    Err(e) => tracing::error!("script output encode: {e}"),
                }
            }
        }

        match encode_app_frame(&snap) {
            Ok(bytes) => {
                let _ = tx.send(bytes);
            }
            Err(e) => tracing::error!("snapshot encode: {e}"),
        }
        // Publish this tick's authoritative prim state for the next slice, so a
        // handler reads the world as it stands after everything applied above.
        scripts.tick(dt, views);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A room name off the wire is client input, and the only bound it had was the
    /// 32 MiB frame limit.
    #[test]
    fn a_room_name_is_bounded_before_it_becomes_a_channel() {
        assert_eq!(room_channel("lobby"), Some("room:lobby".to_string()));
        // Trimmed, so the key matches the one the client's own tab derives from
        // its trimmed input.
        assert_eq!(room_channel("  lobby \n"), Some("room:lobby".to_string()));
        assert_eq!(room_channel(""), None, "an empty room is not a room");
        assert_eq!(room_channel("   "), None);

        let long = "x".repeat(cartopolis_core::MAX_ROOM_LEN + 1);
        assert_eq!(room_channel(&long), None, "an over-long name is refused");
        let at_limit = "x".repeat(cartopolis_core::MAX_ROOM_LEN);
        assert!(
            room_channel(&at_limit).is_some(),
            "the limit itself is allowed"
        );
        // Counted in characters, not bytes, so a name in a non-Latin script gets
        // the same length as one in ASCII.
        let multibyte = "ま".repeat(cartopolis_core::MAX_ROOM_LEN);
        assert!(room_channel(&multibyte).is_some());
    }

    fn setup_test_world() -> (
        Arc<RwLock<SimWorld>>,
        Arc<Mutex<rusqlite::Connection>>,
        tempfile::TempDir,
    ) {
        // Create a temporary test database
        let tempdir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = tempdir.path().join("test.db");
        let conn = crate::db::open_and_migrate(db_path.to_str().unwrap())
            .expect("failed to open and migrate db");
        let prims = crate::db::load_world(&conn).expect("failed to load world");
        let conn = Arc::new(Mutex::new(conn));

        let world = Arc::new(RwLock::new(SimWorld::new(
            prims,
            conn.clone(),
            crate::config::SimConfig::default().world_caps(),
            Default::default(),
            Default::default(),
        )));

        (world, conn, tempdir)
    }

    #[tokio::test]
    async fn test_create_prim_mutation() -> anyhow::Result<()> {
        let (world, _conn, _tempdir) = setup_test_world();

        // Create a prim
        let prim_result = {
            let mut w = world.write().await;
            w.add_prim(
                Vec3::new(10.0, 5.0, 20.0),
                "box",
                cartopolis_core::GeoAnchor {
                    latitude: 0.0,
                    longitude: 0.0,
                },
                None,
                None,
            )
        };

        assert!(prim_result.is_ok(), "prim creation should succeed");
        let created_prim = prim_result.map_err(|e| anyhow::anyhow!(e))?;
        assert!(created_prim.id > 0, "created prim should have positive id");
        assert_eq!(created_prim.shape, "box");
        assert_eq!(created_prim.position, Vec3::new(10.0, 5.0, 20.0));

        // Verify it's in the in-memory list
        let prims_count = {
            let w = world.read().await;
            w.prims().len()
        };
        assert!(prims_count > 0, "world should contain at least one prim");

        Ok(())
    }

    #[tokio::test]
    async fn test_update_prim_mutation() -> anyhow::Result<()> {
        let (world, _conn, _tempdir) = setup_test_world();

        let prim_id = {
            let mut w = world.write().await;
            let prim = w
                .add_prim(
                    Vec3::ZERO,
                    "sphere",
                    cartopolis_core::GeoAnchor {
                        latitude: 0.0,
                        longitude: 0.0,
                    },
                    None,
                    None,
                )
                .map_err(|e| anyhow::anyhow!(e))?;
            prim.id
        };

        // Update the prim
        let update_result = {
            let mut w = world.write().await;
            w.update_prim(
                prim_id,
                Vec3::new(15.0, 10.0, 25.0),
                Vec3::new(45.0, 90.0, 0.0),
                Vec3::new(2.0, 2.0, 2.0),
                [1.0, 0.0, 0.0],
                Some("brick".to_string()),
                "Updated Prim",
                cartopolis_core::PrimSurface::default(),
                cartopolis_core::PrimGeometry::default(),
                cartopolis_core::GeoAnchor {
                    latitude: 0.0,
                    longitude: 0.0,
                },
                None,
                None,
            )
        };

        assert!(update_result.is_ok(), "prim update should succeed");
        let updated_prim = update_result.map_err(|e| anyhow::anyhow!(e))?;
        assert_eq!(updated_prim.id, prim_id);
        assert_eq!(updated_prim.position, Vec3::new(15.0, 10.0, 25.0));
        assert_eq!(updated_prim.rotation, Vec3::new(45.0, 90.0, 0.0));
        assert_eq!(updated_prim.scale, Vec3::new(2.0, 2.0, 2.0));
        assert_eq!(updated_prim.color, [1.0, 0.0, 0.0]);
        assert_eq!(updated_prim.texture_id, Some("brick".to_string()));
        assert_eq!(updated_prim.name, "Updated Prim");

        Ok(())
    }

    #[tokio::test]
    async fn test_delete_prim_mutation() -> anyhow::Result<()> {
        let (world, _conn, _tempdir) = setup_test_world();

        let prim_id = {
            let mut w = world.write().await;
            let prim = w
                .add_prim(
                    Vec3::ZERO,
                    "cube",
                    cartopolis_core::GeoAnchor {
                        latitude: 0.0,
                        longitude: 0.0,
                    },
                    None,
                    None,
                )
                .map_err(|e| anyhow::anyhow!(e))?;
            prim.id
        };

        // Count prims before deletion
        let count_before = {
            let w = world.read().await;
            w.prims().len()
        };

        // Delete the prim
        let delete_result = {
            let mut w = world.write().await;
            w.remove_prim(prim_id)
        };

        assert!(delete_result.is_ok(), "prim deletion should succeed");
        assert!(
            delete_result.map_err(|e| anyhow::anyhow!(e))?,
            "prim should be deleted"
        );

        // Count prims after deletion
        let count_after = {
            let w = world.read().await;
            w.prims().len()
        };

        assert!(
            count_after < count_before,
            "prim count should decrease after deletion"
        );

        Ok(())
    }
}
