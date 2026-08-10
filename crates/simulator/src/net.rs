use crate::config::SimConfig;
use crate::state::SimWorld;
use crate::textures;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, RwLock};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use viberfox_core::{
    decode_app_frame, encode_app_frame, AuthCredential, ChatScope, NetMessage, ProtocolError,
    PROTOCOL_VERSION,
};

/// Chat channel keys and limits (ADR-028).
const GLOBAL_CHANNEL: &str = "geo:";
const LOCAL_CHANNEL: &str = "local";
const MAX_CHAT_LEN: usize = 500;
const CHAT_REPLAY: i64 = 50;
const MIN_LOCAL_INTERVAL: Duration = Duration::from_millis(500);
const MIN_GLOBAL_INTERVAL: Duration = Duration::from_secs(5);
const MIN_ROOM_INTERVAL: Duration = Duration::from_millis(2000);

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
fn authenticate(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    sessions: &crate::auth::Sessions,
    auth: &AuthCredential,
) -> Option<(uuid::Uuid, String)> {
    match auth {
        AuthCredential::Token(token) => sessions.resolve(token),
        AuthCredential::Password { username, password } => {
            // argon2 verify is CPU-bound (~tens of ms) but happens once per
            // connection at a low rate; acceptable inline for v0.
            let guard = conn.lock().ok()?;
            let principal = crate::auth::verify_credentials(&guard, username, password)
                .ok()
                .flatten()?;
            Some((principal, username.clone()))
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

pub async fn handle_connection(
    stream: TcpStream,
    world: Arc<RwLock<SimWorld>>,
    config: Arc<SimConfig>,
    mut snap_rx: broadcast::Receiver<Vec<u8>>,
    tx_snap: broadcast::Sender<Vec<u8>>,
    conn: Arc<Mutex<rusqlite::Connection>>,
    sessions: Arc<crate::auth::Sessions>,
    chat: Arc<crate::chat::ChatHub>,
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
    let (principal, username) = match authenticate(&conn, &sessions, &auth) {
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
    let (chat_tx, mut chat_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    chat.register(avatar_id, chat_tx);
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

    let mut last_intent = Instant::now()
        .checked_sub(MIN_INTENT_INTERVAL)
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

    let mut outcome: anyhow::Result<()> = Ok(());
    loop {
        tokio::select! {
            biased;
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
                                ..
                            } => {
                                if last_intent.elapsed() < MIN_INTENT_INTERVAL {
                                    return Ok(());
                                }
                                last_intent = Instant::now();
                                let mut w = world.write().await;
                                w.apply_intent(
                                    avatar_id, move_x, move_z, display_yaw, flying, fly_up, fly_down,
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
                                match w.remove_prim(prim_id) {
                                    Ok(deleted) => {
                                        if deleted {
                                            // Broadcast PrimRemoved to all clients
                                            let broadcast_msg = encode_app_frame(&NetMessage::PrimRemoved { id: prim_id })?;
                                            let _ = tx_snap.send(broadcast_msg);
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
                                        if last_room.elapsed() < MIN_ROOM_INTERVAL {
                                            return Ok(());
                                        }
                                        last_room = Instant::now();
                                        let channel = format!("room:{room}");
                                        if let Ok(c) = conn.lock() {
                                            let _ = crate::db::insert_chat(&c, &channel, avatar_id as i64, &display_name, now_ms as i64, &body);
                                        }
                                        let frame = encode_app_frame(&NetMessage::ChatMessage {
                                            channel: channel.clone(), sender_id: avatar_id,
                                            sender_name: display_name.clone(), sent_at_ms: now_ms,
                                            body, distance_m: None,
                                        })?;
                                        chat.deliver_room(&channel, &frame);
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
                                            for (id, p) in positions {
                                                let d = (p - sp).length();
                                                if d <= radius {
                                                    let frame = encode_app_frame(&NetMessage::ChatMessage {
                                                        channel: LOCAL_CHANNEL.into(), sender_id: avatar_id,
                                                        sender_name: display_name.clone(), sent_at_ms: now_ms,
                                                        body: body.clone(), distance_m: Some(d),
                                                    })?;
                                                    chat.deliver_to(id, frame);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            NetMessage::ChatSubscribe { room, join } => {
                                let channel = format!("room:{room}");
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
                            NetMessage::PrimRemoved { .. }
                            | NetMessage::WorldSnapshot { .. }
                            | NetMessage::ServerHelloAck { .. }
                            | NetMessage::ServerError { .. }
                            | NetMessage::PrimUpsert { .. }
                            | NetMessage::TextureCatalog { .. }
                            | NetMessage::TextureData { .. }
                            | NetMessage::TextureNotFound { .. }
                            | NetMessage::ChatMessage { .. } => {
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

/// Periodically steps simulation and broadcasts postcard-encoded [`NetMessage::WorldSnapshot`] in app frames.
pub async fn tick_loop(
    world: Arc<RwLock<SimWorld>>,
    config: Arc<SimConfig>,
    tx: broadcast::Sender<Vec<u8>>,
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
        let snap = w.snapshot(tick);
        drop(w);
        match encode_app_frame(&snap) {
            Ok(bytes) => {
                let _ = tx.send(bytes);
            }
            Err(e) => tracing::error!("snapshot encode: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

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
        let conn = Arc::new(Mutex::new(conn));

        let world = Arc::new(RwLock::new(SimWorld::new(
            regions,
            prims,
            50.0,
            conn.clone(),
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
                viberfox_core::GeoAnchor {
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
                    viberfox_core::GeoAnchor {
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
                viberfox_core::PrimSurface::default(),
                viberfox_core::PrimGeometry::default(),
                viberfox_core::GeoAnchor {
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
                    viberfox_core::GeoAnchor {
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
