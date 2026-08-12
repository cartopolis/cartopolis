use crate::config::SimConfig;
use crate::state::SimWorld;
use crate::textures;
use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, ChatScope, NetMessage, ProtocolError,
    PROTOCOL_VERSION,
};
use futures_util::{SinkExt, StreamExt};
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
) -> Option<(uuid::Uuid, String)> {
    match auth {
        AuthCredential::Token(token) => sessions.resolve(token),
        AuthCredential::Password { username, password } => {
            let stored = {
                let guard = conn.lock().ok()?;
                crate::auth::lookup_account(&guard, username).ok()?
            };
            let (principal, phc) = stored?;
            crate::auth::verify_password_bounded(password.clone(), phc)
                .await
                .then(|| (principal, username.clone()))
        }
    }
}

const MAX_FRAME: usize = 32 * 1024 * 1024;
/// ADR-012: simple per-connection rate limits (token-bucket style, fixed interval).
/// Intents are *dropped* over this rate, not queued — and they carry absolute
/// state, so a dropped one loses a key edge outright. At the old 50 ms (one full
/// tick) a press or release could sit unapplied for a whole tick on top of the
/// tick quantisation itself, which read as input lag and stutter. 10 ms still
/// caps a flooding client at 100 `world.write()` locks a second.
const MIN_INTENT_INTERVAL: Duration = Duration::from_millis(10);
const MIN_OBSERVER_INTERVAL: Duration = Duration::from_millis(100);
/// A touch is a deliberate act, so this only has to stop an auto-clicker from
/// making one client's clicks the whole region's script budget.
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

/// Tell every client what a region's budget and usage now are.
///
/// Broadcast rather than answered to the actor alone: the budget is shared, so a
/// prim someone else created is headroom this client no longer has, and finding
/// that out from a refusal is finding out too late.
async fn broadcast_capacity(
    world: &Arc<RwLock<SimWorld>>,
    tx: &broadcast::Sender<Vec<u8>>,
    region_id: i64,
) {
    let Some(usage) = world.read().await.capacity(region_id) else {
        return;
    };
    match encode_app_frame(&capacity_message(&usage)) {
        Ok(bytes) => {
            let _ = tx.send(bytes);
        }
        Err(e) => tracing::error!("capacity encode: {e}"),
    }
}

fn capacity_message(usage: &crate::state::CapacityUsage) -> NetMessage {
    NetMessage::RegionCapacity {
        region_id: usage.region_id,
        prims_used: usage.prims_used,
        prims_max: usage.prims_max,
        scripts_used: usage.scripts_used,
        scripts_max: usage.scripts_max,
        script_memory_bytes: crate::scripting::SCRIPT_MEMORY_BYTES as u32,
    }
}

pub async fn handle_connection(
    stream: TcpStream,
    world: Arc<RwLock<SimWorld>>,
    config: Arc<SimConfig>,
    mut snap_rx: broadcast::Receiver<Vec<u8>>,
    tx_snap: broadcast::Sender<Vec<u8>>,
    conn: Arc<Mutex<rusqlite::Connection>>,
    sessions: Arc<crate::auth::Sessions>,
    chat: Arc<crate::chat::ChatHub>,
    scripts: crate::scripting::ScriptHandle,
) -> anyhow::Result<()> {
    let mut framed = Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(MAX_FRAME)
            .little_endian()
            .new_codec(),
    );

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
        Some(p) => p,
        None => {
            let err = encode_app_frame(&NetMessage::ServerError {
                request_id: 0,
                code: 401,
                message: "authentication failed".into(),
            })?;
            framed.send(Bytes::from(err)).await?;
            return Err(anyhow::anyhow!("authentication failed"));
        }
    };
    let session_token = sessions.issue(principal, &username);

    let avatar_id = {
        let mut w = world.write().await;
        w.spawn_avatar()
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
    })?;
    if let Err(e) = framed.send(Bytes::from(ack)).await {
        let mut w = world.write().await;
        w.remove_avatar(avatar_id);
        return Err(e.into());
    }

    // Chat (ADR-028): register a personal delivery inbox, then replay recent
    // Global scrollback so the tab isn't empty on connect.
    //
    // Two inboxes, both bounded, both drained by this loop — see `chat`'s module
    // note. The bound is the load-bearing part: these are written by *other*
    // connections' tasks, and the only drain is a `select!` arm that shares the
    // loop with `framed.send`, so a client whose window has closed stops draining
    // while its neighbours keep speaking.
    let (mut chat_rx, mut voice_rx) = chat.register(avatar_id);
    for r in chat_history(&conn, GLOBAL_CHANNEL) {
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

    // Region budgets, before any authoring is possible: the editor shows headroom
    // rather than discovering it at the moment a create is refused.
    for usage in world.read().await.all_capacity() {
        let frame = encode_app_frame(&capacity_message(&usage))?;
        framed.send(Bytes::from(frame)).await?;
    }

    let mut last_intent = Instant::now()
        .checked_sub(MIN_INTENT_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_touch = Instant::now()
        .checked_sub(MIN_TOUCH_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_observer = Instant::now()
        .checked_sub(MIN_OBSERVER_INTERVAL)
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
                                );
                            }
                            NetMessage::ObserverUpdate { position } => {
                                if last_observer.elapsed() < MIN_OBSERVER_INTERVAL {
                                    return Ok(());
                                }
                                last_observer = Instant::now();
                                let mut w = world.write().await;
                                w.set_observer(position);
                            }
                            NetMessage::ClientHello { .. } => {
                                tracing::warn!("duplicate hello ignored");
                            }
                            NetMessage::CreatePrim {
                                request_id, region_id, name, position, rotation, scale,
                                color, texture_id, shape, surface, geometry, geo,
                            } => {
                                let mut w = world.write().await;
                                // Insert with a server-assigned id, then apply the full initial
                                // authoring state in one update so it is persisted and broadcast.
                                let created = w.add_prim(region_id, position, &shape, geo).and_then(|prim| {
                                    w.update_prim(
                                        prim.id, position, rotation, scale, color, texture_id,
                                        &name, surface, geometry, geo,
                                    )
                                });
                                match created {
                                    Ok(prim) => {
                                        // Broadcast PrimUpsert to all clients
                                        let broadcast_msg = encode_app_frame(&NetMessage::PrimUpsert { prim })?;
                                        let _ = tx_snap.send(broadcast_msg);
                                        drop(w);
                                        broadcast_capacity(&world, &tx_snap, region_id).await;
                                        tracing::debug!(request_id, region_id, "prim created and broadcast");
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
                            NetMessage::UpdatePrim { request_id, prim_id, position, rotation, scale, color, texture_id, name, surface, geometry, geo } => {
                                let mut w = world.write().await;
                                match w.update_prim(prim_id, position, rotation, scale, color, texture_id, &name, surface, geometry, geo) {
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
                                // Read the region *before* the delete: afterwards the
                                // prim is gone and there is nothing to ask.
                                let region_id = w.prim_region(prim_id);
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
                                            if let Some(region_id) = region_id {
                                                broadcast_capacity(&world, &tx_snap, region_id).await;
                                            }
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
                                let now_ms = now_millis();
                                match scope {
                                    ChatScope::Global => {
                                        if last_global.elapsed() < MIN_GLOBAL_INTERVAL {
                                            return Ok(());
                                        }
                                        last_global = Instant::now();
                                        if let Ok(c) = conn.lock() {
                                            let _ = crate::db::insert_chat(&c, GLOBAL_CHANNEL, avatar_id as i64, &display_name, now_ms as i64, &body);
                                        }
                                        let frame = encode_app_frame(&NetMessage::ChatMessage {
                                            channel: GLOBAL_CHANNEL.into(), sender_id: avatar_id,
                                            sender_name: display_name.clone(), sent_at_ms: now_ms,
                                            body, distance_m: None,
                                        })?;
                                        chat.deliver_all(&frame);
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
                                            let _ = crate::db::insert_chat(&c, &channel, avatar_id as i64, &display_name, now_ms as i64, &body);
                                        }
                                        let frame = encode_app_frame(&NetMessage::ChatMessage {
                                            channel: channel.clone(), sender_id: avatar_id,
                                            sender_name: display_name.clone(), sent_at_ms: now_ms,
                                            body, distance_m: None,
                                        })?;
                                        chat.deliver_room(&channel, crate::chat::Lane::Text, &frame);
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
                                                chat.deliver_to(id, crate::chat::Lane::Text, frame);
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
                                // whether they draw against the region's budget.
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
                                    if w.prim_region(prim_id).is_none() {
                                        Err(format!("prim {prim_id} not found"))
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
                                        if let Some(region_id) = { world.read().await.prim_region(prim_id) } {
                                            broadcast_capacity(&world, &tx_snap, region_id).await;
                                        }
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
                                                chat.deliver_to(id, crate::chat::Lane::Voice, frame);
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
                                        chat.deliver_room_except(&channel, avatar_id, crate::chat::Lane::Voice, &frame);
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
                            | NetMessage::RegionCapacity { .. }
                            | NetMessage::VoiceData { .. } => {
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

    chat.unregister(avatar_id);
    {
        let mut w = world.write().await;
        w.remove_avatar(avatar_id);
    }
    tracing::info!(avatar_id, "avatar removed (disconnect)");

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
        let mut died_regions: Vec<i64> = Vec::new();
        let mut changed: Vec<i64> = Vec::new();
        for cmd in &commands {
            // `vfDie` is handled before the field writes, and its prim is skipped
            // by everything after: a script's last act is often a move *and* a
            // death in the same tick, and broadcasting an update for a prim that
            // is about to be removed leaves the client holding a resurrected copy.
            if let crate::scripting::ScriptCommand::Die { prim_id } = cmd {
                let region_id = w.prim_region(*prim_id);
                match w.remove_prim(*prim_id) {
                    Ok(true) => {
                        died.push(*prim_id);
                        died_regions.extend(region_id);
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
        died_regions.sort_unstable();
        died_regions.dedup();
        for region_id in died_regions {
            broadcast_capacity(&world, &tx, region_id).await;
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
                    Ok(frame) => chat.deliver_to(*id, crate::chat::Lane::Text, frame),
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
        let (regions, prims) = crate::db::load_world(&conn).expect("failed to load world");
        let caps = crate::db::load_region_caps(&conn).expect("failed to load region caps");
        let conn = Arc::new(Mutex::new(conn));

        let world = Arc::new(RwLock::new(SimWorld::new(
            regions,
            prims,
            50.0,
            conn.clone(),
            caps,
            Default::default(),
        )));

        (world, conn, tempdir)
    }

    #[tokio::test]
    async fn test_create_prim_mutation() -> anyhow::Result<()> {
        let (world, _conn, _tempdir) = setup_test_world();

        // Get the default region ID
        let region_id = {
            let w = world.read().await;
            w.regions()[0].id
        };

        // Create a prim
        let prim_result = {
            let mut w = world.write().await;
            w.add_prim(
                region_id,
                Vec3::new(10.0, 5.0, 20.0),
                "box",
                cartopolis_core::GeoAnchor {
                    latitude: 0.0,
                    longitude: 0.0,
                },
            )
        };

        assert!(prim_result.is_ok(), "prim creation should succeed");
        let created_prim = prim_result.map_err(|e| anyhow::anyhow!(e))?;
        assert!(created_prim.id > 0, "created prim should have positive id");
        assert_eq!(created_prim.region_id, region_id);
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

        // Get the default region ID and create a prim first
        let region_id = {
            let w = world.read().await;
            w.regions()[0].id
        };

        let prim_id = {
            let mut w = world.write().await;
            let prim = w
                .add_prim(
                    region_id,
                    Vec3::ZERO,
                    "sphere",
                    cartopolis_core::GeoAnchor {
                        latitude: 0.0,
                        longitude: 0.0,
                    },
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

        // Get the default region ID and create a prim first
        let region_id = {
            let w = world.read().await;
            w.regions()[0].id
        };

        let prim_id = {
            let mut w = world.write().await;
            let prim = w
                .add_prim(
                    region_id,
                    Vec3::ZERO,
                    "cube",
                    cartopolis_core::GeoAnchor {
                        latitude: 0.0,
                        longitude: 0.0,
                    },
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
