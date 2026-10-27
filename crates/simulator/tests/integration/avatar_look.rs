//! Avatar looks (protocol 42): a look one client saves reaches the others, is
//! kept with the account, and comes back on the next sign-in.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, AvatarLook, NetMessage, PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, serve, SimConfig};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Client = Framed<TcpStream, LengthDelimitedCodec>;

async fn connect_auth(addr: &str, user: &str) -> Client {
    let stream = TcpStream::connect(addr).await.unwrap();
    let mut c = Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(32 << 20)
            .little_endian()
            .new_codec(),
    );
    let hello = encode_app_frame(&NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth: AuthCredential::Password {
            username: user.into(),
            password: "dev".into(),
        },
    })
    .unwrap();
    c.send(Bytes::from(hello)).await.unwrap();
    let ack = decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap();
    assert!(matches!(ack, NetMessage::ServerHelloAck { .. }));
    c
}

async fn send(c: &mut Client, msg: &NetMessage) {
    c.send(Bytes::from(encode_app_frame(msg).unwrap()))
        .await
        .unwrap();
}

/// Every look this client is told about within `window`, by avatar.
async fn looks(c: &mut Client, window: Duration) -> Vec<(u64, AvatarLook)> {
    let deadline = tokio::time::Instant::now() + window;
    let mut out = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return out;
        }
        match tokio::time::timeout(left, c.next()).await {
            Ok(Some(Ok(frame))) => {
                if let NetMessage::AvatarLookOf { avatar_id, look } =
                    decode_app_frame(&frame).unwrap()
                {
                    out.push((avatar_id, look));
                }
            }
            Ok(_) | Err(_) => return out,
        }
    }
}

/// Every look this client is told about until `ticks` world snapshots have
/// followed the first one, or `limit` passes. Looks and snapshots share the
/// server's one broadcast channel (`PrimService::broadcast`, the tick's
/// `tx_snap`), so a look sent with or just after the first one arrives before
/// those snapshots do.
async fn looks_settled(c: &mut Client, ticks: usize, limit: Duration) -> Vec<(u64, AvatarLook)> {
    let deadline = tokio::time::Instant::now() + limit;
    let (mut out, mut after) = (Vec::new(), 0);
    while after < ticks {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Some(Ok(frame))) = tokio::time::timeout(left, c.next()).await else {
            break;
        };
        match decode_app_frame(&frame).unwrap() {
            NetMessage::AvatarLookOf { avatar_id, look } => out.push((avatar_id, look)),
            NetMessage::WorldSnapshot { .. } if !out.is_empty() => after += 1,
            _ => {}
        }
    }
    out
}

/// Read until a world snapshot has `avatar` in it (`listed`) or not. Snapshots
/// list only avatars whose client has sent an intent.
async fn until_listed(c: &mut Client, avatar: u64, listed: bool) {
    let found = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(frame)) = c.next().await {
            if let NetMessage::WorldSnapshot { avatars, .. } = decode_app_frame(&frame).unwrap() {
                if avatars.iter().any(|a| a.id == avatar) == listed {
                    return true;
                }
            }
        }
        false
    })
    .await;
    assert_eq!(
        found,
        Ok(true),
        "no snapshot with avatar {avatar} listed = {listed}"
    );
}

async fn boot() -> (tempfile::TempDir, cartopolis_simulator::SimHandle, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("looks.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        for user in ["dev", "other", "late"] {
            ensure_account(&conn, user, "dev").unwrap();
        }
    }
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    let addr = handle.local_addr.to_string();
    (dir, handle, addr)
}

const WINDOW: Duration = Duration::from_millis(800);

fn a_look() -> AvatarLook {
    let mut look = AvatarLook::default_for(true);
    look.hair = Some("afro01".into());
    look.set_face("nose/nose-scale-horiz-decr-incr", 60);
    look
}

#[tokio::test]
async fn a_saved_look_reaches_everyone_and_outlives_the_session() {
    let (_dir, _handle, addr) = boot().await;
    let mut dev = connect_auth(&addr, "dev").await;
    let mut other = connect_auth(&addr, "other").await;
    // Fully in before the look is set, or it is told twice: once as a change and
    // once with the world it is still being handed.
    until_listed(&mut other, u64::MAX, false).await;

    send(&mut dev, &NetMessage::SetAvatarLook { look: a_look() }).await;
    let seen = looks_settled(&mut other, 3, Duration::from_secs(5)).await;
    assert_eq!(seen.len(), 1, "the other client is told once: {seen:?}");
    assert_eq!(seen[0].1, a_look());
    let dev_avatar = seen[0].0;

    // Someone arriving later is handed it with the world.
    let mut late = connect_auth(&addr, "late").await;
    let handed = looks_settled(&mut late, 1, Duration::from_secs(5)).await;
    assert!(
        handed
            .iter()
            .any(|(id, l)| *id == dev_avatar && *l == a_look()),
        "a late arrival gets every online look: {handed:?}"
    );

    // Signing in again wears the saved look without sending it. An intent puts
    // the avatar in the snapshots, so the other client can see it leave.
    send(&mut dev, &standing_still()).await;
    until_listed(&mut other, dev_avatar, true).await;
    drop(dev);
    until_listed(&mut other, dev_avatar, false).await;
    let _dev_again = connect_auth(&addr, "dev").await;
    let again = looks_settled(&mut other, 1, Duration::from_secs(5)).await;
    assert!(
        again.iter().any(|(_, l)| *l == a_look()),
        "the account's look comes back on the next sign-in: {again:?}"
    );
}

fn standing_still() -> NetMessage {
    NetMessage::ClientIntent {
        request_id: 0,
        move_x: 0.0,
        move_z: 0.0,
        display_yaw: 0.0,
        flying: false,
        fly_up: false,
        fly_down: false,
        jump: false,
        emote: None,
        expression: None,
        seated: None,
        geo: None,
    }
}

#[tokio::test]
async fn an_unreasonable_look_is_dropped() {
    let (_dir, _handle, addr) = boot().await;
    let mut dev = connect_auth(&addr, "dev").await;
    let mut other = connect_auth(&addr, "other").await;
    let _ = looks(&mut other, WINDOW).await;
    let mut look = a_look();
    look.age = 9;
    send(&mut dev, &NetMessage::SetAvatarLook { look }).await;
    assert!(
        looks(&mut other, WINDOW).await.is_empty(),
        "a child's body is not relayed"
    );
}

/// Faces (protocol 44): an expression rides the snapshots for as long as it
/// shows, and a tracked face reaches a client standing near, whole frames only.
#[tokio::test]
async fn a_face_reaches_those_near() {
    use cartopolis_core::{Expression, FACE_UNITS};
    let (_dir, _handle, addr) = boot().await;
    let mut dev = connect_auth(&addr, "dev").await;
    let mut other = connect_auth(&addr, "other").await;
    // Both embodied, at the spawn point, so within speaking range.
    send(&mut other, &standing_still()).await;
    let mut laughing = standing_still();
    if let NetMessage::ClientIntent { expression, .. } = &mut laughing {
        *expression = Some(Expression::Laugh);
    }
    send(&mut dev, &laughing).await;
    let shown = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(frame)) = other.next().await {
            if let NetMessage::WorldSnapshot { avatars, .. } = decode_app_frame(&frame).unwrap() {
                if let Some(e) = avatars.iter().find_map(|a| a.expression) {
                    return Some(e);
                }
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    assert_eq!(
        shown.map(|e| e.expression),
        Some(Expression::Laugh),
        "the laugh rides a snapshot"
    );

    // A frame of the wrong length is dropped; a whole one is handed on.
    send(
        &mut dev,
        &NetMessage::SetFaceFrame {
            weights: vec![9; 51],
        },
    )
    .await;
    let face: Vec<u8> = (0..FACE_UNITS as u8).collect();
    send(
        &mut dev,
        &NetMessage::SetFaceFrame {
            weights: face.clone(),
        },
    )
    .await;
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(frame)) = other.next().await {
            if let NetMessage::FaceFrameOf { weights, .. } = decode_app_frame(&frame).unwrap() {
                return Some(weights);
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    assert_eq!(got, Some(face), "the first face to arrive is the whole one");
}
