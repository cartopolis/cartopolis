//! TCP client for `--connect` (ADR-008, ADR-009).

use crate::components::{Avatar, NeedsMeshRebuild, NeedsTextureRefresh, Prim, PrimGeo, PrimShape, Region, RemoteAvatar, RemoteAvatarMotionHint};
use crate::resources::{
    AuthIdentity, AvatarState, CameraState, ConnectAddr, ConnectAuth, GameState, LocalAvatarSimId,
    NetworkMailbox, NetworkSyncState, OnlineSession, OsmTileUrlTemplate, PrimTextureCache,
    TextureLibrary,
};
use crate::systems::user_store::UserStore;
use bevy::ecs::system::SystemParam;

/// Auth-related writeback for `apply_network_snapshot`, grouped so the system
/// stays under Bevy's system-parameter count limit (ADR-016).
#[derive(SystemParam)]
pub struct AuthWriteback<'w> {
    identity: ResMut<'w, AuthIdentity>,
    connect_auth: Option<Res<'w, ConnectAuth>>,
    connect_addr: Option<Res<'w, ConnectAddr>>,
    user_store: ResMut<'w, UserStore>,
}
use crate::systems::avatar::{avatar_facing_yaw_from_camera, wish_dir_camera_relative};
use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::TextureFormat;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashSet;
use std::sync::{mpsc, Arc, Mutex};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use vibe_core::{
    decode_app_frame, encode_app_frame, snap_yaw_continuation, wrap_angle_pi, AuthCredential,
    AvatarStateDto, NetMessage, PrimDto, RegionDto, PROTOCOL_VERSION,
};

const MAX_FRAME: usize = 32 * 1024 * 1024;

pub fn spawn_network_thread(
    mut commands: Commands,
    addr: Res<ConnectAddr>,
    auth: Option<Res<ConnectAuth>>,
    user_store: Res<UserStore>,
) {
    let tile_template = Arc::new(Mutex::new(String::new()));
    let tile_for_thread = tile_template.clone();
    commands.insert_resource(OsmTileUrlTemplate(tile_template));

    let (out_tx, out_rx) = mpsc::channel::<NetMessage>();
    let (intent_tx, intent_rx) = tokio::sync::mpsc::unbounded_channel();
    let connect_to = addr.0.clone();

    // Credential resolved by `login::resolve` in main; default to the solo `dev`
    // account if the resource is somehow absent.
    let auth = auth.map(|a| (*a).clone()).unwrap_or(ConnectAuth {
        credential: AuthCredential::Password {
            username: "dev".to_string(),
            password: "dev".to_string(),
        },
        persist_token: false,
    });
    // Prefer a persisted session token for this server (silent reconnect), with
    // the resolved credential as fallback in case the token is stale (ADR-016).
    let (primary, fallback) = match auth
        .persist_token
        .then(|| user_store.session_token(&connect_to))
        .flatten()
    {
        Some(token) => (AuthCredential::Token(token), Some(auth.credential)),
        None => (auth.credential, None),
    };

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("tokio runtime: {e}");
                return;
            }
        };
        if let Err(e) = rt.block_on(client_loop(
            connect_to,
            primary,
            fallback,
            out_tx,
            intent_rx,
            tile_for_thread,
        )) {
            eprintln!("network client ended: {e:#}");
        }
    });
    commands.insert_resource(NetworkMailbox {
        rx: Mutex::new(out_rx),
    });
    commands.insert_resource(OnlineSession { intent_tx });
    commands.insert_resource(NetworkSyncState::default());
}

async fn client_loop(
    addr: String,
    primary: AuthCredential,
    fallback: Option<AuthCredential>,
    out_tx: mpsc::Sender<NetMessage>,
    mut intent_rx: UnboundedReceiver<NetMessage>,
    tile_template: Arc<Mutex<String>>,
) -> anyhow::Result<()> {
    // Try the primary credential, then the fallback (a stale persisted token
    // gets a fresh password login rather than locking the user out). Each attempt
    // needs its own connection — the server closes on auth failure.
    let attempts: Vec<AuthCredential> = std::iter::once(primary).chain(fallback).collect();
    let mut authed: Option<(Framed<TcpStream, LengthDelimitedCodec>, NetMessage)> = None;
    for (i, credential) in attempts.into_iter().enumerate() {
        let stream = TcpStream::connect(&addr).await?;
        let mut framed = Framed::new(
            stream,
            LengthDelimitedCodec::builder()
                .max_frame_length(MAX_FRAME)
                .little_endian()
                .new_codec(),
        );
        let hello = encode_app_frame(&NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: credential,
        })?;
        framed.send(Bytes::from(hello)).await?;

        let ack_bytes = framed
            .next()
            .await
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("closed before ServerHelloAck"))?;
        let ack_msg = decode_app_frame(&ack_bytes)?;
        match &ack_msg {
            NetMessage::ServerHelloAck {
                tick_hz,
                your_avatar_id,
                osm_tile_url_template,
                ..
            } => {
                if let Ok(mut g) = tile_template.lock() {
                    *g = osm_tile_url_template.clone();
                }
                tracing::info!(tick_hz, your_avatar_id, "handshake ok");
                authed = Some((framed, ack_msg));
                break;
            }
            NetMessage::ServerError { message, .. } => {
                tracing::warn!("auth attempt {i} rejected: {message}");
                continue;
            }
            other => anyhow::bail!("unexpected first server message: {other:?}"),
        }
    }
    let (mut framed, ack_msg) =
        authed.ok_or_else(|| anyhow::anyhow!("authentication failed"))?;
    if out_tx.send(ack_msg).is_err() {
        return Ok(());
    }

    // Request the texture catalog immediately after handshake.
    let cat_req = encode_app_frame(&NetMessage::TextureCatalogRequest)?;
    framed.send(Bytes::from(cat_req)).await?;

    loop {
        tokio::select! {
            biased;
            msg = intent_rx.recv() => {
                match msg {
                    Some(m) => {
                        let b = encode_app_frame(&m)?;
                        framed.send(Bytes::from(b)).await?;
                    }
                    None => break,
                }
            }
            frame = framed.next() => {
                match frame {
                    None => break,
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(bytes)) => {
                        let m = decode_app_frame(&bytes)?;
                        if out_tx.send(m).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Apply network messages (authoritative when online).
pub fn apply_network_snapshot(
    mut commands: Commands,
    mailbox: Option<Res<NetworkMailbox>>,
    mut sync: Option<ResMut<NetworkSyncState>>,
    mut game_state: ResMut<GameState>,
    mut avatar_state: ResMut<AvatarState>,
    mut local_sim_id: ResMut<LocalAvatarSimId>,
    camera_state: Res<CameraState>,
    region_entities: Query<Entity, With<Region>>,
    // Excludes avatars so the `&mut Transform` access is provably disjoint from
    // `avatar_tf` below (prims never carry Avatar/RemoteAvatar).
    mut prim_entities: Query<
        (Entity, &mut Prim, &mut Transform),
        (Without<Avatar>, Without<RemoteAvatar>),
    >,
    mut avatar_tf: Query<&mut Transform, (With<Avatar>, Without<RemoteAvatar>)>,
    mut remote_avatars: Query<(Entity, &mut RemoteAvatar), Without<Avatar>>,
    mut texture_lib: ResMut<TextureLibrary>,
    mut texture_cache: ResMut<PrimTextureCache>,
    mut images: ResMut<Assets<Image>>,
    online: Option<Res<OnlineSession>>,
    mut auth: AuthWriteback,
) {
    let Some(mb) = mailbox else {
        return;
    };
    while let Ok(msg) = mb.lock_rx().try_recv() {
        match msg {
            NetMessage::ServerHelloAck {
                your_avatar_id,
                principal_id,
                username,
                session_token,
                ..
            } => {
                local_sim_id.0 = Some(your_avatar_id);
                tracing::info!(%username, "authenticated");
                auth.identity.principal_id = Some(principal_id);
                auth.identity.username = Some(username);
                // Persist the token for silent reconnect to external sims (ADR-016).
                if let (Some(cauth), Some(caddr)) = (&auth.connect_auth, &auth.connect_addr) {
                    if cauth.persist_token && !session_token.is_empty() {
                        auth.user_store.set_session_token(&caddr.0, &session_token);
                    }
                }
            }
            NetMessage::WorldSnapshot {
                regions,
                prims,
                avatars,
                tick,
            } => {
                tracing::debug!(tick, "world snapshot");
                let repeat_tick = sync
                    .as_ref()
                    .is_some_and(|s| s.received_initial_world);

                if repeat_tick {
                    apply_local_avatar_pose(&avatars, local_sim_id.0, &mut avatar_state);
                    sync_remote_avatar_entities(
                        &mut commands,
                        &avatars,
                        local_sim_id.0,
                        &mut remote_avatars,
                    );
                    continue;
                }

                let region_es: Vec<Entity> = region_entities.iter().collect();
                for e in region_es {
                    commands.entity(e).despawn();
                }
                let prim_es: Vec<Entity> = prim_entities.iter().map(|(e, _, _)| e).collect();
                for e in prim_es {
                    commands.entity(e).despawn();
                }
                let remote_es: Vec<Entity> = remote_avatars.iter().map(|(e, _)| e).collect();
                for e in remote_es {
                    commands.entity(e).despawn();
                }
                game_state.regions_loaded = false;
                game_state.prims_loaded = false;

                for r in regions {
                    commands.spawn(region_from_dto(r));
                }
                game_state.regions_loaded = true;

                for p in prims {
                    spawn_prim_from_dto(&mut commands, p);
                }
                game_state.prims_loaded = true;

                apply_local_avatar_pose_full(
                    &avatars,
                    local_sim_id.0,
                    &mut avatar_state,
                    &mut avatar_tf,
                    camera_state.azimuth,
                );
                sync_remote_avatar_entities(
                    &mut commands,
                    &avatars,
                    local_sim_id.0,
                    &mut remote_avatars,
                );

                if let Some(s) = sync.as_mut() {
                    s.received_initial_world = true;
                }
            }
            NetMessage::PrimRemoved { id } => {
                for (e, p, _) in prim_entities.iter() {
                    if p.id == id {
                        commands.entity(e).despawn();
                        break;
                    }
                }
            }
            NetMessage::PrimUpsert { prim } => {
                // Don't disrupt the prim the local user is actively manipulating: the
                // local copy is authoritative until deselect (and was applied locally
                // when the edit was made). Other prims reflect the server immediately.
                if game_state.selected_prim_id == Some(prim.id) {
                    continue;
                }
                let mut found = false;
                for (entity, mut p, mut tf) in prim_entities.iter_mut() {
                    if p.id == prim.id {
                        *p = prim_from_dto(&prim);
                        *tf = transform_from_dto(&prim);
                        // Geometry may have changed → rebuild mesh; texture/surface →
                        // refresh material (refresh_prim_textures re-applies both).
                        commands
                            .entity(entity)
                            .insert(NeedsMeshRebuild)
                            .insert(NeedsTextureRefresh);
                        // ADR-029: keep the geo anchor in sync so place_geo_prims
                        // re-derives the cell/local transform for the new position.
                        commands.entity(entity).insert(PrimGeo {
                            latitude: prim.geo.latitude,
                            longitude: prim.geo.longitude,
                        });
                        found = true;
                        break;
                    }
                }
                if !found {
                    spawn_prim_from_dto(&mut commands, prim);
                }
            }
            NetMessage::TextureCatalog { textures } => {
                texture_lib.entries = textures.clone();
                if let Some(sess) = online.as_ref() {
                    for entry in &textures {
                        if !texture_cache.handles.contains_key(&entry.id)
                            && !texture_cache.pending.contains(&entry.id)
                        {
                            texture_cache.pending.insert(entry.id.clone());
                            let _ = sess.intent_tx.send(NetMessage::TextureRequest {
                                request_id: 0,
                                texture_id: entry.id.clone(),
                            });
                        }
                    }
                }
                tracing::info!(count = textures.len(), "texture catalog received");
            }
            NetMessage::TextureData { texture_id, png_bytes, .. } => {
                texture_cache.pending.remove(&texture_id);
                match decode_png_to_image(&png_bytes) {
                    Some(img) => {
                        let handle = images.add(img);
                        texture_cache.handles.insert(texture_id.clone(), handle);
                        for (entity, prim, _) in prim_entities.iter() {
                            if prim.texture_id.as_deref() == Some(&texture_id) {
                                commands.entity(entity).insert(NeedsTextureRefresh);
                            }
                        }
                        tracing::debug!(id = %texture_id, "texture loaded");
                    }
                    None => tracing::warn!(id = %texture_id, "failed to decode texture PNG"),
                }
            }
            NetMessage::TextureNotFound { texture_id, .. } => {
                texture_cache.pending.remove(&texture_id);
                tracing::warn!(id = %texture_id, "texture not found on server");
            }
            _ => {}
        }
    }
}

/// Updates authoritative position and replicated yaw. Does not touch [`AvatarState::online_tank_yaw`]
/// (snapshot reconciliation there would cancel A/D before the server applies the next intent).
fn apply_local_avatar_pose(
    avatars: &[AvatarStateDto],
    local_id: Option<u64>,
    avatar_state: &mut AvatarState,
) {
    let Some(a) = local_avatar_dto(avatars, local_id) else {
        return;
    };
    avatar_state.position = a.position;
    avatar_state.sim_facing_yaw = snap_yaw_continuation(avatar_state.sim_facing_yaw, a.yaw);
}

fn apply_local_avatar_pose_full(
    avatars: &[AvatarStateDto],
    local_id: Option<u64>,
    avatar_state: &mut AvatarState,
    avatar_tf: &mut Query<&mut Transform, (With<Avatar>, Without<RemoteAvatar>)>,
    camera_azimuth: f32,
) {
    let Some(a) = local_avatar_dto(avatars, local_id) else {
        return;
    };
    let pi = std::f32::consts::PI;
    avatar_state.position = a.position;
    avatar_state.display_position = a.position;
    avatar_state.sim_facing_yaw = snap_yaw_continuation(avatar_state.sim_facing_yaw, a.yaw);
    let face = avatar_facing_yaw_from_camera(camera_azimuth);
    avatar_state.online_tank_yaw = wrap_angle_pi(face - pi);
    if let Ok(mut tf) = avatar_tf.single_mut() {
        tf.translation = a.position;
        tf.rotation = Quat::from_rotation_y(face);
    }
}

fn local_avatar_dto<'a>(avatars: &'a [AvatarStateDto], local_id: Option<u64>) -> Option<&'a AvatarStateDto> {
    if let Some(id) = local_id {
        avatars.iter().find(|a| a.id == id)
    } else {
        avatars.first()
    }
}

fn sync_remote_avatar_entities(
    commands: &mut Commands,
    avatars: &[AvatarStateDto],
    local_id: Option<u64>,
    remote_query: &mut Query<(Entity, &mut RemoteAvatar), Without<Avatar>>,
) {
    let expected: HashSet<u64> = avatars
        .iter()
        .filter(|a| Some(a.id) != local_id)
        .map(|a| a.id)
        .collect();

    let stale: Vec<Entity> = remote_query
        .iter()
        .filter(|(_, r)| !expected.contains(&r.sim_id))
        .map(|(e, _)| e)
        .collect();
    for e in stale {
        commands.entity(e).despawn();
    }

    for a in avatars {
        if Some(a.id) == local_id {
            continue;
        }
        let mut found = false;
        for (_, mut r) in remote_query.iter_mut() {
            if r.sim_id == a.id {
                r.net_position = a.position;
                r.net_yaw = snap_yaw_continuation(r.net_yaw, a.yaw);
                found = true;
                break;
            }
        }
        if !found {
            let y = snap_yaw_continuation(0.0, a.yaw);
            commands.spawn((
                RemoteAvatar {
                    sim_id: a.id,
                    net_position: a.position,
                    net_yaw: y,
                },
                RemoteAvatarMotionHint::default(),
                Transform::from_translation(a.position)
                    .with_rotation(Quat::from_rotation_y(wrap_angle_pi(y)))
                    .with_scale(Vec3::splat(0.02)),
            ));
        }
    }
}

fn region_from_dto(r: RegionDto) -> Region {
    Region {
        id: r.id,
        name: r.name,
        latitude: r.latitude,
        longitude: r.longitude,
        tile_x: r.tile_x,
        tile_y: r.tile_y,
        tile_z: r.tile_z,
        sim_origin: Some(Vec3::new(r.sim_x, r.sim_y, r.sim_z)),
    }
}

fn prim_from_dto(p: &PrimDto) -> Prim {
    Prim {
        id: p.id,
        region_id: p.region_id,
        name: p.name.clone(),
        shape: PrimShape::from_str(&p.shape),
        color: Color::srgb(p.color[0], p.color[1], p.color[2]),
        texture_id: p.texture_id.clone(),
        path_cut_begin: p.path_cut_begin,
        path_cut_end: p.path_cut_end,
        hollow: p.hollow,
        twist_begin: p.twist_begin,
        twist_end: p.twist_end,
        taper_x: p.taper_x,
        taper_y: p.taper_y,
        top_shear_x: p.top_shear_x,
        top_shear_y: p.top_shear_y,
        slice_begin: p.slice_begin,
        slice_end: p.slice_end,
        surface: p.surface,
    }
}

fn transform_from_dto(p: &PrimDto) -> Transform {
    Transform::from_translation(p.position)
        .with_rotation(Quat::from_euler(
            EulerRot::XYZ,
            p.rotation.x,
            p.rotation.y,
            p.rotation.z,
        ))
        .with_scale(p.scale)
}

/// Spawn a prim entity from a DTO with its absolute geo anchor (ADR-029);
/// `prim_geo::place_geo_prims` derives the `GridCell` + local `Transform` from
/// the current OSM anchor.
fn spawn_prim_from_dto(commands: &mut Commands, p: PrimDto) {
    let geo = PrimGeo {
        latitude: p.geo.latitude,
        longitude: p.geo.longitude,
    };
    commands.spawn((prim_from_dto(&p), transform_from_dto(&p), geo));
}

fn decode_png_to_image(png_bytes: &[u8]) -> Option<Image> {
    let dynamic = image::load_from_memory(png_bytes).ok()?;
    let rgba = dynamic.to_rgba8();
    let (w, h) = rgba.dimensions();
    // Mipmapped + repeat addressing (repeat_u/v > 1 tiles). MAIN_WORLD keeps the
    // mip-0 CPU bytes available for `create_egui_texture_handles`.
    Some(crate::systems::rendering::mipmapped_image(
        w,
        h,
        rgba.into_raw(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        crate::systems::rendering::repeat_linear_sampler(),
    ))
}

/// Creates egui thumbnail handles for any texture in the library that doesn't have one yet.
/// Runs every Update frame; no-ops once all entries are covered.
pub fn create_egui_texture_handles(
    mut texture_lib: ResMut<TextureLibrary>,
    texture_cache: Res<PrimTextureCache>,
    images: Res<Assets<Image>>,
    egui_manager: Res<super::egui_manager::EguiManager>,
) {
    let entries: Vec<String> = texture_lib
        .entries
        .iter()
        .filter(|e| !texture_lib.egui_handles.contains_key(&e.id))
        .map(|e| e.id.clone())
        .collect();

    for id in entries {
        let Some(img_handle) = texture_cache.handles.get(&id) else {
            continue;
        };
        let Some(img) = images.get(img_handle) else {
            continue;
        };
        let Some(data) = &img.data else {
            continue;
        };
        let w = img.width();
        let h = img.height();
        // `data` now holds the full mip chain; the thumbnail only needs mip 0
        // (the first w·h·4 bytes).
        let mip0_len = (w * h * 4) as usize;
        if data.len() < mip0_len {
            continue;
        }
        let pixels: Vec<egui::Color32> = data[..mip0_len]
            .chunks_exact(4)
            .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect();
        let color_image = egui::ColorImage::new([w as usize, h as usize], pixels);
        let handle = egui_manager.ctx.load_texture(&id, color_image, egui::TextureOptions::LINEAR);
        texture_lib.egui_handles.insert(id, handle);
    }
}

pub fn send_network_intent(
    online: Option<Res<OnlineSession>>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    camera_state: Res<crate::resources::CameraState>,
    egui_manager: Res<super::egui_manager::EguiManager>,
    edit_dialog: Res<crate::resources::EditDialogState>,
) {
    let Some(sess) = online else {
        return;
    };
    if camera_state.mode != crate::resources::CameraMode::Avatar {
        return;
    }

    // When the UI owns the keyboard (text field, edit dialog, Ctrl chord), send a zero
    // intent rather than skipping: the sim persists the last velocity, so we must
    // actively tell it to stop instead of leaving the avatar drifting.
    let blocked = egui_manager.ui_owns_keyboard(edit_dialog.visible, &keyboard_input);
    let move_forward = !blocked
        && (keyboard_input.pressed(KeyCode::KeyW) || keyboard_input.pressed(KeyCode::ArrowUp));
    let move_backward = !blocked
        && (keyboard_input.pressed(KeyCode::KeyS) || keyboard_input.pressed(KeyCode::ArrowDown));
    let move_left = !blocked
        && (keyboard_input.pressed(KeyCode::KeyA) || keyboard_input.pressed(KeyCode::ArrowLeft));
    let move_right = !blocked
        && (keyboard_input.pressed(KeyCode::KeyD) || keyboard_input.pressed(KeyCode::ArrowRight));
    let fly_up = !blocked && keyboard_input.pressed(KeyCode::Space);
    let fly_down = !blocked
        && (keyboard_input.pressed(KeyCode::ShiftLeft)
            || keyboard_input.pressed(KeyCode::ShiftRight));

    let az = camera_state.azimuth;
    let v = wish_dir_camera_relative(az, move_forward, move_backward, move_left, move_right);
    let display_yaw = avatar_facing_yaw_from_camera(az);

    let _ = sess.intent_tx.send(NetMessage::ClientIntent {
        request_id: 0,
        move_x: v.x,
        move_z: v.z,
        display_yaw,
        fly_up,
        fly_down,
    });
}

pub fn send_observer_update(
    online: Option<Res<OnlineSession>>,
    avatar_state: Res<AvatarState>,
    camera_state: Res<crate::resources::CameraState>,
) {
    let Some(sess) = online else {
        return;
    };
    if camera_state.mode != crate::resources::CameraMode::Avatar {
        return;
    }
    let _ = sess.intent_tx.send(NetMessage::ObserverUpdate {
        position: avatar_state.position,
    });
}
