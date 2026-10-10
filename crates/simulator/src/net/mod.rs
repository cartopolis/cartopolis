//! One client's connection, and the tick loop that feeds every connection.
//!
//! # Layout
//!
//! [`handle_connection`] owns a connection's life: the handshake, the replay a
//! newcomer gets, and the select loop that interleaves what the client sends with
//! what the world broadcasts. What the client *sends* is classified once, by
//! [`ClientRequest`]'s `TryFrom` (`request.rs`), and handed to a handler per
//! message in the submodule for its concern — `avatar`, `prims`, `textures`,
//! `chat`, `voice`, `moderation`, `e2ee`, `calls`. Every handler takes the same
//! [`ConnCtx`], which is everything a connection carries between messages: who
//! it is, its transport, the shared server state and its own rate limiters.
//!
//! The split is by concern rather than by anything structural: the handlers were
//! arms of one 1,300-line `match` inside one function, where a `return Ok(())`
//! meant "skip this message" and a `?` meant "end the connection" and nothing
//! but reading the whole function said which arm owned which local.

use crate::config::SimConfig;
use crate::db::{self, SharedConn};
use crate::state::SimWorld;
use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, NetMessage, ProtocolError, PROTOCOL_VERSION,
};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use glam::Vec3;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, RwLock};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

mod avatar;
mod calls;
mod chat;
mod communities;
mod contacts;
mod e2ee;
mod hand;
mod moderation;
mod prims;
pub mod request;
mod textures;
mod voice;

pub use request::ClientRequest;

/// Chat channel keys and limits (ADR-028).
const GLOBAL_CHANNEL: &str = "geo:";
const LOCAL_CHANNEL: &str = "local";
const MAX_CHAT_LEN: usize = 500;
const CHAT_REPLAY: i64 = 50;
const MIN_LOCAL_INTERVAL: Duration = Duration::from_millis(500);
const MIN_GLOBAL_INTERVAL: Duration = Duration::from_secs(5);

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

/// Private chats: key-package claims refill at one per this interval per
/// connection, up to [`CLAIM_BURST`] at once. Each claim deletes a package from
/// somebody else's stock, so an unthrottled loop could leave a person
/// impossible to add to a chat until they next connect and top up.
///
/// A bucket rather than a plain interval because starting a group claims for
/// every member at once: a fixed half-second gap dropped all but the first
/// claim of a group without a word, which reads as those people having no
/// app that can chat privately.
const MIN_CLAIM_INTERVAL: Duration = Duration::from_millis(500);
/// Claims one connection may make back to back — a group's worth of people.
const CLAIM_BURST: u32 = 12;

/// Shortest gap between two `CallJoin`s from one connection. Each join builds
/// two WebRTC connections on the forwarder, which is the expensive thing a
/// client could ask for in a loop.
const MIN_CALL_JOIN_INTERVAL: Duration = Duration::from_secs(1);
/// Contacts: person lookups refill at one per this interval per connection, up
/// to [`LOOKUP_BURST`] at once. A lookup says whether a username exists, so an
/// unthrottled loop would list every account on the server.
const MIN_LOOKUP_INTERVAL: Duration = Duration::from_millis(1000);
/// Lookups one connection may make back to back.
const LOOKUP_BURST: u32 = 10;
/// Shortest gap between two `WherePeople` from one connection. The World view
/// asks every few seconds; anything faster is dropped unanswered, and the next
/// ask brings the view up to date.
const MIN_WHERE_INTERVAL: Duration = Duration::from_millis(900);
/// Most people one `WherePeople` is answered for. A chat bigger than this shows
/// its first ones; the ask is a list of names, not a way to sweep the server.
const MAX_WHERE_PEOPLE: usize = 32;

/// Longest username the moderation messages will act on.
///
/// `SetBlocked`/`ReportUser` carry a `String` straight off the wire whose only
/// other bound is `MAX_FRAME`, and both end up as a row in SQLite. Comfortably past any account name — `accounts.username`
/// is what these have to match, and nobody signs up with a paragraph.
const MAX_USERNAME_LEN: usize = 64;

/// Milliseconds since the Unix epoch, for chat timestamps.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Recent scrollback for a channel (empty on any lock/query error).
async fn chat_history(conn: &SharedConn, channel: &str) -> Vec<crate::db::ChatRow> {
    let channel = channel.to_string();
    db::call(conn, move |c| {
        crate::db::recent_chat(c, &channel, CHAT_REPLAY)
    })
    .await
    .ok()
    .and_then(|rows| rows.ok())
    .unwrap_or_default()
}

/// An account's block list as it stands now, lowercased for matching.
///
/// Read fresh by the private-chat sync and the call join rather than taken from
/// the list the connection loaded at the handshake: a block made since must hold
/// on those paths too.
fn blocked_lowercase(
    c: &rusqlite::Connection,
    principal: &str,
) -> std::collections::HashSet<String> {
    crate::db::blocked_usernames(c, principal)
        .unwrap_or_default()
        .into_iter()
        .map(|u| u.to_lowercase())
        .collect()
}

/// A token bucket carried as "the instant the next token may be spent".
///
/// Adding an interval per spend and clamping the gate to at most `burst`
/// intervals in the past is a token bucket with no arithmetic to get wrong: the
/// clamp is the refill, the addition is the spend. Returns whether a token was
/// spent.
fn take_token(gate: &mut Instant, interval: Duration, burst: u32) -> bool {
    let now = Instant::now();
    let floor = now.checked_sub(interval * burst).unwrap_or(now);
    if *gate < floor {
        *gate = floor;
    }
    let ok = *gate <= now;
    if ok {
        *gate += interval;
    }
    ok
}

/// A `ServerError` answering `request_id`.
fn server_error(request_id: u32, code: u32, message: impl Into<String>) -> NetMessage {
    NetMessage::ServerError {
        request_id,
        code,
        message: message.into(),
    }
}

/// Resolve a hello credential to `(principal, username)` (ADR-016). `None` = reject.
///
/// The database is touched for exactly as long as it takes to read one row, on a
/// blocking thread. An earlier version passed the live `MutexGuard` into
/// `verify_credentials`, so the argon2 verify — tens of milliseconds, by design —
/// ran with the SQLite mutex held, and a handful of sockets sending nothing but
/// `ClientHello` could stall everything else that needed the database before
/// authenticating as anybody. Hashing happens after the guard is gone, on a
/// blocking thread, behind a concurrency cap (`auth::verify_password_bounded`).
async fn authenticate(
    conn: &SharedConn,
    sessions: &crate::auth::Sessions,
    auth: &AuthCredential,
) -> Result<(uuid::Uuid, String), AuthRefusal> {
    match auth {
        AuthCredential::Token(token) => sessions.resolve(token).ok_or(AuthRefusal::BadCredential),
        AuthCredential::Password { username, password } => {
            let name = username.clone();
            let stored = db::call(conn, move |c| crate::auth::lookup_credential(c, &name))
                .await
                .map_err(|_| AuthRefusal::BadCredential)?
                .map_err(|_| AuthRefusal::BadCredential)?;
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

/// Tell every client what the world's budget and usage now are.
///
/// Broadcast rather than answered to the actor alone: the budget is shared, so a
/// prim someone else created is headroom this client no longer has, and finding
/// that out from a refusal is finding out too late.
pub(crate) async fn broadcast_capacity(
    world: &Arc<RwLock<SimWorld>>,
    tx: &broadcast::Sender<Bytes>,
) {
    let usage = world.read().await.capacity();
    match encode_app_frame(&capacity_message(&usage)) {
        Ok(bytes) => {
            let _ = tx.send(Bytes::from(bytes));
        }
        Err(e) => tracing::error!("capacity encode: {e}"),
    }
}

/// What an account may do, as its client is told it.
fn permissions_message(perms: cartopolis_core::permissions::PermissionSet) -> NetMessage {
    NetMessage::Permissions {
        granted: perms.slugs().into_iter().map(str::to_string).collect(),
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
    /// The account and database, so the contacts who can see this account is
    /// online hear that it went (`contacts`).
    presence: Option<(String, SharedConn)>,
}

impl Drop for AvatarGuard {
    fn drop(&mut self) {
        // Unregister on a never-registered id is a map remove of nothing.
        self.chat.unregister(self.avatar_id);
        // Spawned rather than done here: it reads the database, which a `Drop`
        // on a runtime worker must not wait on. After `unregister`, so this
        // connection no longer counts as the account being online.
        if let Some((account, conn)) = self.presence.take() {
            let chat = self.chat.clone();
            if let Ok(h) = tokio::runtime::Handle::try_current() {
                h.spawn(async move {
                    contacts::push_contacts_to_contacts(&conn, &chat, &account).await;
                });
            }
        }
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
    truth: Option<cartopolis_core::PrimDto>,
    message: String,
) -> anyhow::Result<()> {
    framed
        .send(Bytes::from(encode_app_frame(&server_error(
            request_id, 403, message,
        ))?))
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

/// The per-connection rate limiters, each carried as the instant its last
/// action happened (or, for the two buckets, the instant the next may).
struct RateGates {
    last_intent: Instant,
    last_touch: Instant,
    last_local: Instant,
    last_global: Instant,
    last_voice: Instant,
    /// The script-save bucket ([`take_token`]).
    script_store: Instant,
    /// The key-package claim bucket ([`take_token`]).
    claim: Instant,
    /// The person-lookup bucket ([`take_token`]).
    lookup: Instant,
    last_call_join: Instant,
    last_where: Instant,
    /// The hand bucket (`hand::on_hand`, [`take_token`]).
    hand: Instant,
}

impl RateGates {
    /// Every gate open: a connection's first action of each kind is never
    /// throttled, and each bucket starts full.
    fn open() -> Self {
        let ago = |d: Duration| Instant::now().checked_sub(d).unwrap_or_else(Instant::now);
        Self {
            last_intent: ago(MIN_INTENT_INTERVAL),
            last_touch: ago(MIN_TOUCH_INTERVAL),
            last_local: ago(MIN_LOCAL_INTERVAL),
            last_global: ago(MIN_GLOBAL_INTERVAL),
            last_voice: ago(MIN_VOICE_INTERVAL),
            script_store: ago(MIN_SCRIPT_STORE_INTERVAL * SCRIPT_STORE_BURST),
            claim: ago(MIN_CLAIM_INTERVAL * CLAIM_BURST),
            lookup: ago(MIN_LOOKUP_INTERVAL * LOOKUP_BURST),
            last_call_join: ago(MIN_CALL_JOIN_INTERVAL),
            last_where: ago(MIN_WHERE_INTERVAL),
            hand: ago(hand::HAND_INTERVAL * hand::HAND_BURST),
        }
    }
}

/// Everything one connection carries between messages, handed to every handler.
///
/// The identity fields are fixed at the handshake. `actor.perms` is read there
/// and then kept current by `grants` rather than re-read per edit (`UpdatePrim`
/// arrives at the gizmo's drag rate): an operator changing a role reaches every
/// connected account through `roles::refresh_grants`, which updates this copy
/// and tells the client (`NetMessage::Permissions`).
struct ConnCtx<T> {
    framed: T,
    world: Arc<RwLock<SimWorld>>,
    config: Arc<SimConfig>,
    conn: SharedConn,
    sessions: Arc<crate::auth::Sessions>,
    chat: Arc<crate::chat::ChatHub>,
    scripts: crate::scripting::ScriptHandle,
    calls: Option<crate::calls::CallsHandle>,
    prims: crate::prims::PrimService,
    /// Who this connection is to the prim service: the account and what its
    /// roles allow.
    actor: crate::prims::Actor,
    principal: uuid::Uuid,
    /// `principal` as the string the database keys accounts by.
    principal_key: String,
    /// The account's username, which is also its name in chat.
    display_name: String,
    avatar_id: u64,
    /// Built once per connection rather than once per message: a few dozen short
    /// strings into a `HashSet`, against a `ChatSend` path that would otherwise
    /// rebuild it at typing speed.
    filter: cartopolis_core::Filter,
    gates: RateGates,
    /// Whether this client is being looked at (`NetMessage::SetPresence`).
    ///
    /// Starts `true`: a client that never sends the message — every build older
    /// than protocol 18, and every desktop — behaves exactly as it always did.
    /// Only the snapshot arm of the select loop reads this; chat, voice, prim
    /// updates and everything else are delivered to an idle client unchanged,
    /// which is the entire point of holding the connection open.
    presence_active: bool,
    /// This connection's private-chat device, once registered (protocol 25). Every
    /// other private-chat message is ignored until it is: the server keys
    /// invitations and cursors by device, and a connection with none has no keys
    /// to read anything with anyway.
    e2ee_device: Option<uuid::Uuid>,
    /// This connection's place in a call (protocol 26). One call at a time;
    /// dropping the seat leaves the call, which is also what happens when the
    /// connection ends and this goes out of scope.
    call_seat: Option<crate::calls::Seat>,
    /// The push endpoint this connection last registered (protocol 29), so
    /// `SetPushEndpoint { endpoint: None }` knows which one to take back.
    push_endpoint: Option<String>,
}

impl<T: FrameTransport> ConnCtx<T> {
    /// Encode and send one message to this client.
    async fn send(&mut self, msg: &NetMessage) -> anyhow::Result<()> {
        self.framed
            .send(Bytes::from(encode_app_frame(msg)?))
            .await?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_connection<T: FrameTransport>(
    mut framed: T,
    world: Arc<RwLock<SimWorld>>,
    config: Arc<SimConfig>,
    mut snap_rx: broadcast::Receiver<Bytes>,
    conn: SharedConn,
    sessions: Arc<crate::auth::Sessions>,
    chat: Arc<crate::chat::ChatHub>,
    scripts: crate::scripting::ScriptHandle,
    calls: Option<crate::calls::CallsHandle>,
    prims: crate::prims::PrimService,
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
        let err = encode_app_frame(&server_error(
            0,
            1,
            format!("version {protocol_version} not supported"),
        ))?;
        framed.send(Bytes::from(err)).await?;
        return Err(ProtocolError::UnsupportedVersion(protocol_version).into());
    }

    // ADR-016: authenticate before any world traffic. No snapshot or mutating
    // message is served until this succeeds.
    let (principal, username) = match authenticate(&conn, &sessions, &auth).await {
        Ok(p) => p,
        Err(refusal) => {
            let message = refusal.message();
            let err = encode_app_frame(&server_error(0, 401, message))?;
            framed.send(Bytes::from(err)).await?;
            return Err(anyhow::anyhow!("{message}"));
        }
    };
    let session_token = sessions.issue(principal, &username);

    // What this connection may do (`roles::effective`; see `ConnCtx`'s note on
    // why this is read once). A database error grants nothing.
    let perms = db::call(&conn, move |c| crate::roles::effective(c, principal))
        .await
        .ok()
        .and_then(|p| p.ok())
        .unwrap_or_default();
    let principal_key = principal.to_string();
    // Kept current for the rest of the session (`roles::refresh_grants`).
    let mut grants = chat.watch_grants(&principal_key, perms);

    let avatar_id = {
        let mut w = world.write().await;
        let id = w.spawn_avatar();
        // Who this avatar is, for anything that has to name it — `sensor` hands
        // the name to a script so a prim can greet whoever walked up.
        w.set_avatar_name(id, &username);
        id
    };
    // From here on, every exit — `?`, break, or unwind — retires the avatar.
    let _cleanup = AvatarGuard {
        world: world.clone(),
        chat: chat.clone(),
        avatar_id,
        presence: Some((principal.to_string(), conn.clone())),
    };
    tracing::info!(%principal, %username, avatar_id, "client authenticated");
    // Display name for chat (the ack moves `username`).
    let display_name = username.clone();
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
        let principal_id = principal_key.clone();
        db::call(&conn, move |c| {
            crate::db::blocked_usernames(c, &principal_id)
        })
        .await
        .ok()
        .and_then(|list| list.ok())
        .unwrap_or_default()
    };
    let blocked_lower: std::collections::HashSet<String> =
        blocked.iter().map(|u| u.to_lowercase()).collect();
    let filter = config.moderation_filter();
    let (mut chat_rx, mut voice_rx) = chat.register(avatar_id, &principal_key, blocked.clone());

    // The acknowledgement goes out only once this connection is registered with
    // the chat hub. Reading the block list is a hop to the blocking pool, and a
    // client that already held its ack could speak to — or be spoken to by — a
    // neighbour in that gap, which the hub would silently not deliver.
    let ack = encode_app_frame(&NetMessage::ServerHelloAck {
        session_id: uuid::Uuid::new_v4(),
        tick_hz: config.tick_hz,
        your_avatar_id: avatar_id,
        principal_id: principal,
        username,
        session_token,
        osm_tile_url_template: config.osm_tile_url_template.clone(),
        api_base_url: config.resolved_api_base_url(),
        server_name: config.server_name(),
    })?;
    framed.send(Bytes::from(ack)).await?;

    let mut ctx = ConnCtx {
        framed,
        world,
        config,
        conn,
        sessions,
        chat,
        scripts,
        calls,
        prims,
        actor: crate::prims::Actor::with_permissions(principal_key.clone(), perms),
        principal,
        principal_key,
        display_name,
        avatar_id,
        filter,
        gates: RateGates::open(),
        presence_active: true,
        e2ee_device: None,
        call_seat: None,
        push_endpoint: None,
    };

    ctx.send(&permissions_message(perms)).await?;
    ctx.send(&NetMessage::BlockedList { usernames: blocked })
        .await?;
    // Contacts: this connection's own list, and "online" to the contacts who can
    // see it. The own list goes straight onto this connection rather than
    // through the inbox: reading it is a hop to the blocking pool, and through
    // the inbox it raced the loop below, so a client could get its first list
    // *after* the answer to a message it sent later — and read that stale list
    // as the reply. Every later update still arrives through the inbox.
    if let Some(msg) = contacts::contacts_message(&ctx.conn, &ctx.chat, &ctx.principal_key).await {
        ctx.send(&msg).await?;
    }
    contacts::push_contacts_to_contacts(&ctx.conn, &ctx.chat, &ctx.principal_key).await;
    for r in chat_history(&ctx.conn, GLOBAL_CHANNEL).await {
        if blocked_lower.contains(&r.sender_name.to_lowercase()) {
            continue;
        }
        ctx.send(&NetMessage::ChatMessage {
            channel: GLOBAL_CHANNEL.into(),
            sender_id: r.sender_id as u64,
            sender_name: r.sender_name,
            sent_at_ms: r.sent_at_ms as u64,
            body: r.body,
            distance_m: None,
        })
        .await?;
    }

    // The budget, before any authoring is possible: the editor shows headroom
    // rather than discovering it at the moment a create is refused.
    {
        let usage = ctx.world.read().await.capacity();
        ctx.send(&capacity_message(&usage)).await?;
    }

    // The one full snapshot this connection gets. The client builds its prim
    // entities from the first `WorldSnapshot` it sees and ignores the prim list
    // in every later one, so this is where the world arrives; the broadcast that
    // follows carries avatars only (`SimWorld::avatar_snapshot`).
    //
    // Sent here rather than left to the next tick for a second reason: it is up
    // to a whole tick period sooner, so a new arrival's world is drawn on join
    // instead of up to 50 ms after it. Ordering is safe because this write goes
    // out on `framed` before the select loop below reads its first broadcast
    // frame, and a broadcast snapshot that was already queued cannot overtake it.
    {
        let frame = {
            let w = ctx.world.read().await;
            encode_app_frame(&w.snapshot(0))?
        };
        ctx.framed.send(Bytes::from(frame)).await?;
    }

    let mut outcome: anyhow::Result<()> = Ok(());
    loop {
        // No `biased;`. It polled `framed.next()` first, and the rate limiters in
        // the handlers return without awaiting anything — so a client sending
        // continuously kept the first arm permanently ready and its own snapshot
        // and chat arms were never polled at all. `biased` earns its keep when one
        // arm is a shutdown signal that must win; none of these is, so the random
        // polling order is the one that keeps a busy client receiving.
        tokio::select! {
            incoming = ctx.framed.next() => {
                match incoming {
                    None => break,
                    Some(Err(e)) => {
                        outcome = Err(e.into());
                        break;
                    }
                    Some(Ok(bytes)) => {
                        // An error here *leaves the loop* instead of returning out
                        // of `handle_connection`. Every fallible step in a handler
                        // — a truncated postcard body, an encode, a send — used to
                        // skip the teardown at the end of this function, so one
                        // malformed frame from an authenticated client leaked its
                        // avatar into `SimWorld` permanently. `snapshot` replicates
                        // every avatar to every client, so the leak was visible to
                        // everyone as a frozen ghost. The other select arms use
                        // exactly this shape, and `_cleanup` covers the rest.
                        //
                        // A handler that decides to skip a message (a rate limit, a
                        // refused room) returns `Ok(())`; only a failure it cannot
                        // answer is an `Err`.
                        if let Err(e) = on_frame(&mut ctx, &bytes).await {
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
                        // told a prim changed, it just does not need everyone's
                        // position twenty times a second while it is off screen.
                        // `frame_kind` reads the header rather than decoding the
                        // payload — decoding a snapshot to discard it would cost
                        // more than sending it.
                        if !ctx.presence_active
                            && cartopolis_core::protocol::frame_kind(&bytes)
                                == Some(cartopolis_core::protocol::MessageKind::WorldSnapshot)
                        {
                            continue;
                        }
                        if let Err(e) = ctx.framed.send(bytes).await {
                            outcome = Err(e.into());
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // An operator changed what this account may do. Both halves at once:
            // the gate this connection's edits are checked against, and the
            // client's copy, which decides what it offers.
            Ok(()) = grants.changed() => {
                let now = *grants.borrow_and_update();
                ctx.actor.perms = now;
                if let Err(e) = ctx.send(&permissions_message(now)).await {
                    outcome = Err(e);
                    break;
                }
            }
            chat_frame = chat_rx.recv() => {
                match chat_frame {
                    Some(frame) => {
                        if let Err(e) = ctx.framed.send(Bytes::from(frame)).await {
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
                        if let Err(e) = ctx.framed.send(Bytes::from(frame)).await {
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

/// Decode one frame from the client and hand it to its handler.
async fn on_frame<T: FrameTransport>(ctx: &mut ConnCtx<T>, bytes: &[u8]) -> anyhow::Result<()> {
    let msg = decode_app_frame(bytes)?;
    match ClientRequest::try_from(msg) {
        Ok(request) => dispatch(ctx, request).await,
        Err(other) => {
            tracing::debug!(
                kind = ?cartopolis_core::message_kind(&other),
                "ignored server-to-client message from client"
            );
            Ok(())
        }
    }
}

/// Every client request, to its handler. Exhaustive on purpose — see
/// [`request`]'s module note.
async fn dispatch<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request: ClientRequest,
) -> anyhow::Result<()> {
    use crate::prims::{PrimFields, PrimSeed};
    match request {
        ClientRequest::Hello => {
            tracing::warn!("duplicate hello ignored");
            Ok(())
        }
        ClientRequest::Intent {
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
        } => {
            avatar::on_intent(
                ctx,
                avatar::Intent {
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
                },
            )
            .await
        }
        ClientRequest::Teleport { position } => avatar::on_teleport(ctx, position).await,
        ClientRequest::SetPresence { active } => {
            avatar::on_presence(ctx, active);
            Ok(())
        }
        ClientRequest::CreatePrim {
            request_id,
            name,
            position,
            rotation,
            scale,
            color,
            texture_id,
            shape,
            surface,
            geometry,
            geo,
            particles,
            model,
        } => {
            let seed = PrimSeed {
                position,
                shape,
                geo,
            };
            let fields = PrimFields {
                position,
                rotation,
                scale,
                color,
                texture_id,
                name,
                surface,
                geometry,
                geo,
                particles,
                model,
            };
            prims::on_create(ctx, request_id, seed, fields).await
        }
        ClientRequest::UpdatePrim {
            request_id,
            prim_id,
            position,
            rotation,
            scale,
            color,
            texture_id,
            name,
            surface,
            geometry,
            geo,
            particles,
            model,
        } => {
            let fields = PrimFields {
                position,
                rotation,
                scale,
                color,
                texture_id,
                name,
                surface,
                geometry,
                geo,
                particles,
                model,
            };
            prims::on_update(ctx, request_id, prim_id, fields).await
        }
        ClientRequest::DeletePrim {
            request_id,
            prim_id,
        } => prims::on_delete(ctx, request_id, prim_id).await,
        ClientRequest::SetPrimVideo {
            request_id,
            prim_id,
            video,
        } => prims::on_set_video(ctx, request_id, prim_id, video).await,
        ClientRequest::SetPrimPhysical {
            request_id,
            prim_id,
            physical,
        } => prims::on_set_physical(ctx, request_id, prim_id, physical).await,
        ClientRequest::SetPushEndpoint { endpoint } => {
            calls::on_set_push_endpoint(ctx, endpoint).await
        }
        ClientRequest::GetPrimScript {
            request_id,
            prim_id,
        } => prims::on_get_script(ctx, request_id, prim_id).await,
        ClientRequest::SetPrimScript {
            request_id,
            prim_id,
            source,
        } => prims::on_set_script(ctx, request_id, prim_id, source).await,
        ClientRequest::TouchPrim {
            request_id,
            prim_id,
        } => prims::on_touch(ctx, request_id, prim_id).await,
        ClientRequest::HandPose { pose } => hand::on_hand(ctx, pose).await,
        ClientRequest::TextureCatalogRequest => textures::on_catalog(ctx).await,
        ClientRequest::TextureRequest {
            request_id,
            texture_id,
        } => textures::on_texture(ctx, request_id, texture_id).await,
        ClientRequest::ChatSend { scope, body } => chat::on_send(ctx, scope, body).await,
        ClientRequest::VoiceFrame {
            scope,
            seq,
            payload,
        } => voice::on_frame(ctx, scope, seq, payload).await,
        ClientRequest::SetBlocked { username, blocked } => {
            moderation::on_set_blocked(ctx, username, blocked).await
        }
        ClientRequest::ReportUser {
            request_id,
            username,
            reason,
            note,
            message,
            asset,
        } => {
            moderation::on_report(
                ctx,
                moderation::Report {
                    request_id,
                    subject: username,
                    reason,
                    note,
                    message,
                    asset,
                },
            )
            .await
        }
        ClientRequest::DeleteAccount {
            request_id,
            password,
        } => moderation::on_delete_account(ctx, request_id, password).await,
        ClientRequest::E2eeRegisterDevice {
            request_id,
            device,
            key_packages,
        } => e2ee::on_register_device(ctx, request_id, device, key_packages).await,
        ClientRequest::E2eeClaimKeyPackages {
            request_id,
            address,
        } => e2ee::on_claim_key_packages(ctx, request_id, address).await,
        ClientRequest::E2eeCreateGroup {
            request_id,
            group,
            direct_with,
        } => e2ee::on_create_group(ctx, request_id, group, direct_with).await,
        ClientRequest::E2eeSend {
            request_id,
            group,
            epoch,
            commit,
            message,
            welcome,
        } => {
            e2ee::on_send(
                ctx,
                e2ee::Outgoing {
                    request_id,
                    group,
                    epoch,
                    commit,
                    message,
                    welcome,
                },
            )
            .await
        }
        ClientRequest::E2eeSync {
            request_id,
            cursors,
        } => e2ee::on_sync(ctx, request_id, cursors).await,
        ClientRequest::CallJoin {
            request_id,
            group,
            offer,
        } => calls::on_join(ctx, request_id, group, offer).await,
        ClientRequest::CallAnswer { group, sdp } => {
            calls::on_answer(ctx, group, sdp);
            Ok(())
        }
        ClientRequest::CallLeave { group } => {
            calls::on_leave(ctx, group);
            Ok(())
        }
        ClientRequest::CallDecline { group } => calls::on_decline(ctx, group).await,
        ClientRequest::RequestContact {
            request_id,
            username,
            link_code,
        } => contacts::on_request(ctx, request_id, username, link_code).await,
        ClientRequest::AnswerContact {
            request_id,
            username,
            accept,
        } => contacts::on_answer(ctx, request_id, username, accept).await,
        ClientRequest::RemoveContact { username } => contacts::on_remove(ctx, username).await,
        ClientRequest::SetContactSettings {
            policy,
            show_online,
            notify_requests,
            separate_new_accounts,
            reset_link,
        } => {
            contacts::on_settings(
                ctx,
                contacts::Settings {
                    policy,
                    show_online,
                    notify_requests,
                    separate_new_accounts,
                    reset_link,
                },
            )
            .await
        }
        ClientRequest::LookupPerson {
            request_id,
            username,
            avatar_id,
        } => contacts::on_lookup(ctx, request_id, username, avatar_id).await,
        ClientRequest::GetNearbyPeople { request_id } => contacts::on_nearby(ctx, request_id).await,
        ClientRequest::WherePeople {
            request_id,
            usernames,
        } => contacts::on_where(ctx, request_id, usernames).await,
        ClientRequest::Community(msg) => communities::dispatch(ctx, msg).await,
    }
}

/// How often script writes are pushed to SQLite, in ticks. A script that moves a
/// prim does so every tick; persisting each one would be 20 writes a second per
/// script for state that is about to change again. The in-memory value is what
/// clients see, so the only thing this cadence costs is how much motion a crash
/// rewinds.
const SCRIPT_FLUSH_TICKS: u64 = 40;

/// How long a tick takes, summarised into the log instead of into a metrics
/// system this server does not have.
///
/// The number worth watching is not the average — it is the tail, and where in
/// the tick it is spent. Two numbers are kept because they answer different
/// questions: `locked` is how long the world's write lock was held, which is the
/// time every connected client's own work is queued behind, and `total` is the
/// whole tick including the encoding and fan-out that happen after the lock is
/// dropped. A `total` near the period with a small `locked` is a busy but
/// healthy server; a large `locked` is the thing that makes other connections
/// wait.
///
/// Reported once a window rather than once a tick: at 20 Hz a per-tick line is
/// 72,000 lines an hour, which is a log nobody reads.
struct TickMeter {
    window: u64,
    ticks: u64,
    total_sum: Duration,
    total_max: Duration,
    locked_sum: Duration,
    locked_max: Duration,
    /// Ticks in this window whose total exceeded the tick period — i.e. ticks
    /// that could not have kept the cadence on their own.
    over: u64,
}

impl TickMeter {
    fn new(period: Duration) -> Self {
        // A window of about thirty seconds at any tick rate, so the cadence of
        // the log line does not change when `tick_hz` does.
        let window = ((30.0 / period.as_secs_f64()).round() as u64).max(1);
        Self {
            window,
            ticks: 0,
            total_sum: Duration::ZERO,
            total_max: Duration::ZERO,
            locked_sum: Duration::ZERO,
            locked_max: Duration::ZERO,
            over: 0,
        }
    }

    fn record(&mut self, total: Duration, locked: Duration, period: Duration) {
        self.ticks += 1;
        self.total_sum += total;
        self.locked_sum += locked;
        self.total_max = self.total_max.max(total);
        self.locked_max = self.locked_max.max(locked);
        if total > period {
            self.over += 1;
        }
        if self.ticks < self.window {
            return;
        }
        let n = self.ticks.max(1) as u32;
        // `info` when a tick in the window overran the period, `debug` otherwise:
        // a healthy server should not narrate itself, but a server that is
        // missing its cadence should say so without anyone having to raise the
        // log level first.
        let mean_total_ms = (self.total_sum / n).as_secs_f64() * 1000.0;
        let mean_locked_ms = (self.locked_sum / n).as_secs_f64() * 1000.0;
        let max_total_ms = self.total_max.as_secs_f64() * 1000.0;
        let max_locked_ms = self.locked_max.as_secs_f64() * 1000.0;
        let over = self.over;
        let ticks = self.ticks;
        let budget_ms = period.as_secs_f64() * 1000.0;
        if over > 0 {
            tracing::info!(
                ticks,
                over,
                budget_ms,
                mean_total_ms,
                max_total_ms,
                mean_locked_ms,
                max_locked_ms,
                "tick timing"
            );
        } else {
            tracing::debug!(
                ticks,
                budget_ms,
                mean_total_ms,
                max_total_ms,
                mean_locked_ms,
                max_locked_ms,
                "tick timing"
            );
        }
        *self = Self::new(period);
    }
}

/// Periodically steps simulation and broadcasts postcard-encoded [`NetMessage::WorldSnapshot`] in app frames.
pub async fn tick_loop(
    world: Arc<RwLock<SimWorld>>,
    config: Arc<SimConfig>,
    tx: broadcast::Sender<Bytes>,
    // Owned, not shared: the host's command receiver is `!Sync`, so an `Arc` of it
    // could not cross a `tokio::spawn`. This is the one place that drains it, which
    // is the same reason [`ScriptHost`] and [`ScriptHandle`] are separate types.
    scripts: crate::scripting::ScriptHost,
    chat: Arc<crate::chat::ChatHub>,
    // What writes script changes back to the database, off this loop.
    prims: crate::prims::PrimService,
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
    let mut meter = TickMeter::new(period);
    // The flush in flight, if any. One at a time: a second flush started while
    // the first is still writing would only queue behind it on the prim writer
    // lock, and a slow disk would pile them up without bound.
    let mut flushing: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        interval.tick().await;
        tick += 1;
        let tick_began = Instant::now();
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
        let lock_taken = Instant::now();
        w.step(dt);

        // Scripts are applied at the tick barrier: everything the host produced
        // since the last tick lands in one batch, before the snapshot that
        // carries it, so a client never sees half a script's frame of work.
        let commands = scripts.drain();
        let mut script_frames: Vec<Vec<u8>> = Vec::new();
        let mut died: Vec<i64> = Vec::new();
        let mut changed: Vec<i64> = Vec::new();
        let mut toned: Vec<i64> = Vec::new();
        for cmd in &commands {
            // `die` is handled before the field writes, and its prim is skipped
            // by everything after: a script's last act is often a move *and* a
            // death in the same tick, and broadcasting an update for a prim that
            // is about to be removed leaves the client holding a resurrected copy.
            //
            // Removed from the world now and from the database at the next flush
            // (`SimWorld::retire_prim`): this loop holds the world lock, and the
            // disk is not something it may wait on while it does.
            if let crate::scripting::ScriptCommand::Die { prim_id } = cmd {
                if w.retire_prim(*prim_id) {
                    died.push(*prim_id);
                    scripts.unload(*prim_id);
                    match encode_app_frame(&NetMessage::PrimRemoved { id: *prim_id }) {
                        Ok(bytes) => script_frames.push(bytes),
                        Err(e) => tracing::error!("script prim removed encode: {e}"),
                    }
                }
                continue;
            }
            if died.contains(&cmd.prim_id()) {
                continue;
            }
            if let crate::scripting::ScriptCommand::SetTone { prim_id, .. } = cmd {
                toned.push(*prim_id);
            }
            if w.apply_script_command(cmd).is_some() {
                changed.push(cmd.prim_id());
            }
        }
        // One `PrimTone` per prim per tick, carrying the last tone its script
        // set: a handler following a hand may set several in one slice, and
        // only where the tone ended up is worth sending.
        toned.sort_unstable();
        toned.dedup();
        for prim_id in toned {
            let Some(prim) = w.prim(prim_id) else {
                continue;
            };
            match encode_app_frame(&NetMessage::PrimTone {
                prim_id,
                tone: prim.tone,
            }) {
                Ok(bytes) => script_frames.push(bytes),
                Err(e) => tracing::error!("prim tone encode: {e}"),
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
        // Where the physics step moved the physical prims, as one message for all
        // of them (protocol 34). A prim a script also changed this tick has had
        // its upsert queued above; its pose here is the step's, which is newer.
        let bodies = w.take_motion();
        if !bodies.is_empty() {
            match encode_app_frame(&NetMessage::PrimMotion { tick, bodies }) {
                Ok(bytes) => script_frames.push(bytes),
                Err(e) => tracing::error!("prim motion encode: {e}"),
            }
        }
        let snap = w.avatar_snapshot(tick);
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
                .filter_map(|id| w.embodied_position(id).map(|p| (id, p)))
                .collect()
        };
        let locked_for = lock_taken.elapsed();
        drop(w);

        // Persisting what scripts changed is a task of its own, which takes the
        // world lock only to copy the dirty prims out and does its writing with
        // the lock released (`PrimService::flush`). It used to run right here,
        // inside the lock, so every commit's trip to the disk was a pause for
        // every connected client.
        if tick % SCRIPT_FLUSH_TICKS == 0 && flushing.as_ref().is_none_or(|f| f.is_finished()) {
            flushing = Some(prims.spawn_flush());
        }

        for bytes in script_frames {
            let _ = tx.send(Bytes::from(bytes));
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
                    Ok(frame) => chat.deliver_to(
                        *id,
                        name.as_str(),
                        crate::chat::Lane::Text,
                        Bytes::from(frame),
                    ),
                    Err(e) => tracing::error!("script say encode: {e}"),
                }
            }
        }
        // A one-shot sound goes to every connection, like a console line: the
        // client knows where the prim is in its own frame and the server does
        // not (an avatar's sim position is metres in *its* client's frame), so
        // the range test is the client's. Capped per script at the host
        // (`MAX_SOUNDS_PER_TICK`). A prim that died this tick makes no sound.
        for cmd in &commands {
            if let crate::scripting::ScriptCommand::PlaySound { prim_id, sound } = cmd {
                if died.contains(prim_id) {
                    continue;
                }
                match encode_app_frame(&NetMessage::PrimSound {
                    prim_id: *prim_id,
                    sound: *sound,
                }) {
                    Ok(frame) => {
                        let _ = tx.send(Bytes::from(frame));
                    }
                    Err(e) => tracing::error!("script sound encode: {e}"),
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
                        let _ = tx.send(Bytes::from(frame));
                    }
                    Err(e) => tracing::error!("script output encode: {e}"),
                }
            }
        }

        match encode_app_frame(&snap) {
            Ok(bytes) => {
                let _ = tx.send(Bytes::from(bytes));
            }
            Err(e) => tracing::error!("snapshot encode: {e}"),
        }
        // Publish this tick's authoritative prim state for the next slice, so a
        // handler reads the world as it stands after everything applied above.
        scripts.tick(dt, views);

        meter.record(tick_began.elapsed(), locked_for, period);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prims::{Actor, ModelCheck, PrimError, PrimFields, PrimSeed, PrimService};
    use cartopolis_core::GeoAnchor;

    /// The bucket lets a burst through, then one per interval. A full bucket is
    /// `burst` banked intervals *plus* the one due now — the rule the three
    /// hand-written copies this replaced all had, kept as it was.
    #[test]
    fn a_token_bucket_allows_its_burst_and_then_refuses() {
        let interval = Duration::from_secs(60);
        let mut gate = Instant::now()
            .checked_sub(interval * 3)
            .unwrap_or_else(Instant::now);
        for n in 0..4 {
            assert!(
                take_token(&mut gate, interval, 3),
                "token {n} of a full bucket"
            );
        }
        assert!(
            !take_token(&mut gate, interval, 3),
            "a fifth token inside a minute"
        );
    }

    /// A prim service over a throwaway database. The script host is returned so
    /// it outlives the test — dropping it joins its thread.
    fn setup() -> (
        PrimService,
        Arc<RwLock<SimWorld>>,
        SharedConn,
        crate::scripting::ScriptHost,
        tempfile::TempDir,
    ) {
        let tempdir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = tempdir.path().join("test.db");
        let conn = crate::db::open_and_migrate(db_path.to_str().unwrap())
            .expect("failed to open and migrate db");
        let prims = crate::db::load_world(&conn).expect("failed to load world");
        let conn = Arc::new(std::sync::Mutex::new(conn));
        let world = Arc::new(RwLock::new(SimWorld::new(
            prims,
            crate::config::SimConfig::default().world_caps(),
            Default::default(),
            Default::default(),
        )));
        let host = crate::scripting::ScriptHost::spawn();
        let (tx, _) = broadcast::channel(64);
        let service = PrimService::new(world.clone(), conn.clone(), tx, host.handle());
        (service, world, conn, host, tempdir)
    }

    fn seed(shape: &str) -> PrimSeed {
        PrimSeed {
            position: Vec3::new(10.0, 5.0, 20.0),
            shape: shape.to_string(),
            geo: GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
        }
    }

    async fn create(service: &PrimService, actor: &Actor, shape: &str) -> cartopolis_core::PrimDto {
        service
            .create(
                actor,
                seed(shape),
                |base| Ok(PrimFields::of(base)),
                ModelCheck::Skip,
            )
            .await
            .expect("prim creation should succeed")
    }

    #[tokio::test]
    async fn test_create_prim_mutation() -> anyhow::Result<()> {
        let (service, world, conn, _host, _tempdir) = setup();
        let alice = Actor::account("alice", false);
        let created = create(&service, &alice, "box").await;
        assert!(created.id > 0, "created prim should have positive id");
        assert_eq!(created.shape, "box");
        assert_eq!(created.position, Vec3::new(10.0, 5.0, 20.0));

        // In the world, owned by its builder, and in the database.
        {
            let w = world.read().await;
            assert_eq!(w.prims().len(), 1);
            assert_eq!(w.prim_owner(created.id), Some("alice"));
        }
        let stored = crate::db::load_world(&conn.lock().expect("db"))?;
        assert_eq!(
            stored.len(),
            1,
            "a create is in the database before it is announced"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_update_prim_mutation() -> anyhow::Result<()> {
        let (service, _world, _conn, _host, _tempdir) = setup();
        let alice = Actor::account("alice", false);
        let prim_id = create(&service, &alice, "sphere").await.id;

        let updated = service
            .update(
                &alice,
                prim_id,
                |base| {
                    let mut f = PrimFields::of(base);
                    f.position = Vec3::new(15.0, 10.0, 25.0);
                    f.rotation = Vec3::new(45.0, 90.0, 0.0);
                    f.scale = Vec3::new(2.0, 2.0, 2.0);
                    f.color = [1.0, 0.0, 0.0];
                    f.texture_id = Some("brick".to_string());
                    f.name = "Updated Prim".to_string();
                    Ok(f)
                },
                ModelCheck::Skip,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert_eq!(updated.id, prim_id);
        assert_eq!(updated.position, Vec3::new(15.0, 10.0, 25.0));
        assert_eq!(updated.rotation, Vec3::new(45.0, 90.0, 0.0));
        assert_eq!(updated.scale, Vec3::new(2.0, 2.0, 2.0));
        assert_eq!(updated.color, [1.0, 0.0, 0.0]);
        assert_eq!(updated.texture_id, Some("brick".to_string()));
        assert_eq!(updated.name, "Updated Prim");

        // Somebody else may not, an admin may, and a prim that is not there is
        // not there for everybody.
        let bob = Actor::account("bob", false);
        let refused = service
            .update(
                &bob,
                prim_id,
                |base| Ok(PrimFields::of(base)),
                ModelCheck::Skip,
            )
            .await;
        assert_eq!(refused.err(), Some(PrimError::NotYours(prim_id)));
        let admin = Actor::account("root", true);
        assert!(service
            .update(
                &admin,
                prim_id,
                |base| Ok(PrimFields::of(base)),
                ModelCheck::Skip
            )
            .await
            .is_ok());
        let missing = service
            .update(
                &bob,
                9_999,
                |base| Ok(PrimFields::of(base)),
                ModelCheck::Skip,
            )
            .await;
        assert_eq!(missing.err(), Some(PrimError::NotFound(9_999)));
        Ok(())
    }

    #[tokio::test]
    async fn test_delete_prim_mutation() -> anyhow::Result<()> {
        let (service, world, conn, _host, _tempdir) = setup();
        let alice = Actor::account("alice", false);
        let prim_id = create(&service, &alice, "cube").await.id;
        let count_before = world.read().await.prims().len();

        assert_eq!(
            service.delete(&Actor::account("bob", false), prim_id).await,
            Err(PrimError::NotYours(prim_id)),
            "somebody else's prim stays"
        );
        service
            .delete(&alice, prim_id)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let count_after = world.read().await.prims().len();
        assert!(
            count_after < count_before,
            "prim count should decrease after deletion"
        );
        assert!(crate::db::load_world(&conn.lock().expect("db"))?.is_empty());
        assert_eq!(
            service.delete(&alice, prim_id).await,
            Err(PrimError::NotFound(prim_id))
        );
        Ok(())
    }

    /// Script writes reach the database through the flush, which takes them out
    /// of the world under the lock and writes them with it released; a script's
    /// own deletion goes the same way.
    #[tokio::test]
    async fn a_flush_persists_script_writes_and_deaths() -> anyhow::Result<()> {
        let (service, world, conn, _host, _tempdir) = setup();
        let server = Actor::server();
        let moved = create(&service, &server, "box").await.id;
        let doomed = create(&service, &server, "box").await.id;
        {
            let mut w = world.write().await;
            let to = Vec3::new(1.0, 2.0, 3.0);
            assert!(w
                .apply_script_command(&crate::scripting::ScriptCommand::SetPos {
                    prim_id: moved,
                    pos: to,
                })
                .is_some());
            assert!(w.retire_prim(doomed));
        }
        service.flush().await;
        let stored = crate::db::load_world(&conn.lock().expect("db"))?;
        assert_eq!(stored.len(), 1, "the retired prim's row is gone");
        assert_eq!(stored[0].id, moved);
        assert_eq!(stored[0].position, Vec3::new(1.0, 2.0, 3.0));
        Ok(())
    }
}
