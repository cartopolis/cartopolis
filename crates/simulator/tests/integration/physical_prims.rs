//! Physical prims (protocol 34), end to end over the wire: a player switches
//! their ball to physical, everybody is told, it falls and comes to rest, an
//! avatar's kick sends it across the pitch, somebody else may not switch it,
//! and the flag outlives a restart.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, Emote, GeoAnchor, NetMessage, PrimBody,
    PrimGeometry, PrimSurface, PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, serve, SimConfig, SimHandle};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Client = Framed<TcpStream, LengthDelimitedCodec>;

const PITCH: GeoAnchor = GeoAnchor {
    latitude: 53.20662,
    longitude: 6.59188,
};

async fn server(db: &str) -> SimHandle {
    let config = SimConfig {
        database_path: db.to_string(),
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    };
    serve(Arc::new(config)).await.unwrap()
}

async fn connect(handle: &SimHandle, username: &str) -> Client {
    let stream = TcpStream::connect(handle.local_addr).await.unwrap();
    let mut c = Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(32 << 20)
            .little_endian()
            .new_codec(),
    );
    send(
        &mut c,
        &NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: AuthCredential::Password {
                username: username.into(),
                password: "pw".into(),
            },
        },
    )
    .await;
    let ack = decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap();
    assert!(matches!(ack, NetMessage::ServerHelloAck { .. }), "{ack:?}");
    c
}

async fn send(c: &mut Client, msg: &NetMessage) {
    c.send(Bytes::from(encode_app_frame(msg).unwrap()))
        .await
        .unwrap();
}

async fn next<T>(c: &mut Client, mut f: impl FnMut(NetMessage) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = tokio::time::timeout(left, c.next())
            .await
            .expect("an answer in time")
            .unwrap()
            .unwrap();
        if let Some(v) = f(decode_app_frame(&frame).unwrap()) {
            return v;
        }
    }
}

fn ball_body(m: NetMessage, id: i64) -> Option<PrimBody> {
    match m {
        NetMessage::PrimMotion { bodies, .. } => bodies.into_iter().find(|b| b.id == id),
        _ => None,
    }
}

/// An intent that stands still at `geo`, facing `yaw`, optionally kicking.
fn stand(geo: GeoAnchor, yaw: f32, emote: Option<Emote>) -> NetMessage {
    NetMessage::ClientIntent {
        request_id: 0,
        move_x: 0.0,
        move_z: 0.0,
        display_yaw: yaw,
        flying: false,
        fly_up: false,
        fly_down: false,
        jump: false,
        emote,
        seated: None,
        geo: Some(geo),
    }
}

#[tokio::test]
async fn a_ball_falls_rests_is_kicked_and_stays_physical() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("physics.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, "alice", "pw").unwrap();
        ensure_account(&conn, "bob", "pw").unwrap();
    }
    let s = server(&db).await;
    let mut a = connect(&s, "alice").await;
    let mut b = connect(&s, "bob").await;

    // Alice makes a football two metres up.
    send(
        &mut a,
        &NetMessage::CreatePrim {
            request_id: 1,
            name: "ball".into(),
            position: glam::Vec3::new(0.0, 2.0, 0.0),
            rotation: glam::Vec3::ZERO,
            scale: glam::Vec3::splat(0.22),
            color: [1.0, 1.0, 1.0],
            texture_id: None,
            shape: "sphere".into(),
            surface: PrimSurface::default(),
            geometry: PrimGeometry::default(),
            geo: PITCH,
            particles: None,
            model: None,
        },
    )
    .await;
    let id = next(&mut a, |m| match m {
        NetMessage::PrimUpsert { prim } => Some(prim.id),
        _ => None,
    })
    .await;

    // Bob may not make Alice's ball physical.
    send(
        &mut b,
        &NetMessage::SetPrimPhysical {
            request_id: 2,
            prim_id: id,
            physical: true,
        },
    )
    .await;
    let code = next(&mut b, |m| match m {
        NetMessage::ServerError {
            request_id: 2,
            code,
            ..
        } => Some(code),
        _ => None,
    })
    .await;
    assert_eq!(code, 403);

    // Alice may, and Bob is told.
    send(
        &mut a,
        &NetMessage::SetPrimPhysical {
            request_id: 3,
            prim_id: id,
            physical: true,
        },
    )
    .await;
    next(&mut b, |m| match m {
        NetMessage::PrimUpsert { prim } if prim.id == id && prim.physical => Some(()),
        _ => None,
    })
    .await;

    // It falls, and comes to rest on the ground: one last pose marked resting.
    let rest = next(&mut b, |m| ball_body(m, id).filter(|b| b.resting)).await;
    assert!(
        (rest.altitude - 0.11).abs() < 0.03,
        "a 22 cm ball rests with its centre at 0.11 m, not {}",
        rest.altitude
    );

    // Bob walks up beside it — half a metre west — facing east, and kicks.
    let (_, m_lng) = cartopolis_core::degree_scale_m(PITCH.latitude);
    let beside = GeoAnchor {
        latitude: rest.geo.latitude,
        longitude: rest.geo.longitude - 0.5 / m_lng,
    };
    let east = std::f32::consts::FRAC_PI_2;
    send(&mut b, &stand(beside, east, None)).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    send(&mut b, &stand(beside, east, Some(Emote::Kick))).await;
    let flying = next(&mut a, |m| ball_body(m, id).filter(|b| b.velocity.x > 5.0)).await;
    assert!(
        flying.velocity.z.abs() < 1.0,
        "kicked east, went {:?}",
        flying.velocity
    );

    // Gone from the world and back: the flag is in the row.
    drop((a, b));
    s.shutdown();
    let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
    let prims = cartopolis_simulator::db::load_world(&conn).unwrap();
    let ball = prims.iter().find(|p| p.id == id).unwrap();
    assert!(ball.physical);
}
