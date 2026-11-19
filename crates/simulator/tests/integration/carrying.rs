//! Picking things up and throwing them (protocol 48), end to end over the
//! wire: a ball within reach is picked up, carried in front of the avatar as it
//! walks and thrown; the sim refuses one out of reach, one too big, one fixed in
//! place and one already in somebody's hands; and leaving, a teleport and the
//! ball being switched off each let go of it.

use std::time::Duration;

use cartopolis_core::{
    decode_app_frame, AuthCredential, GeoAnchor, NetMessage, PrimBody, PrimGeometry, PrimSurface,
    PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, SimHandle};
use futures_util::StreamExt;
use glam::Vec3;
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use super::physical_prims::{ball_body, next, send, server, stand, Client, PITCH};

/// Connect and sign in, handing back the avatar's id.
async fn connect(handle: &SimHandle, username: &str) -> (Client, u64) {
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
    let NetMessage::ServerHelloAck { your_avatar_id, .. } = ack else {
        panic!("{ack:?}");
    };
    (c, your_avatar_id)
}

/// A server with three accounts, in its own directory.
async fn world() -> (tempfile::TempDir, SimHandle) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("carry.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        for name in ["alice", "bob", "carol"] {
            ensure_account(&conn, name, "pw").unwrap();
        }
    }
    let s = server(&db).await;
    (dir, s)
}

/// `geo` moved `east` and `north` metres.
fn at(geo: GeoAnchor, east: f64, north: f64) -> GeoAnchor {
    let (m_lat, m_lng) = cartopolis_core::degree_scale_m(geo.latitude);
    GeoAnchor {
        latitude: geo.latitude + north / m_lat,
        longitude: geo.longitude + east / m_lng,
    }
}

/// Metres east of `a` that `b` is.
fn east_of(a: GeoAnchor, b: GeoAnchor) -> f64 {
    let (_, m_lng) = cartopolis_core::degree_scale_m(a.latitude);
    (b.longitude - a.longitude) * m_lng
}

/// Put an avatar somewhere and face it `yaw`.
async fn place(c: &mut Client, geo: GeoAnchor, yaw: f32) {
    send(
        c,
        &NetMessage::Teleport {
            request_id: 0,
            geo,
            alt_m: 0.0,
        },
    )
    .await;
    send(c, &stand(geo, yaw, None)).await;
}

/// Make a prim and hand back its id, once `c` has been told of it.
async fn make(
    c: &mut Client,
    name: &str,
    shape: &str,
    scale: Vec3,
    y: f32,
    geo: GeoAnchor,
    physical: bool,
) -> i64 {
    send(
        c,
        &NetMessage::CreatePrim {
            request_id: 1,
            name: name.into(),
            position: Vec3::new(0.0, y, 0.0),
            rotation: Vec3::ZERO,
            scale,
            color: [1.0, 1.0, 1.0],
            texture_id: None,
            shape: shape.into(),
            surface: PrimSurface::default(),
            geometry: PrimGeometry::default(),
            geo,
            particles: None,
            model: None,
            draft: false,
        },
    )
    .await;
    let id = next(c, |m| match m {
        NetMessage::PrimUpsert { prim } if prim.name == name => Some(prim.id),
        _ => None,
    })
    .await;
    if physical {
        send(
            c,
            &NetMessage::SetPrimPhysical {
                request_id: 2,
                prim_id: id,
                physical: true,
            },
        )
        .await;
        next(c, |m| match m {
            NetMessage::PrimUpsert { prim } if prim.id == id && prim.physical => Some(()),
            _ => None,
        })
        .await;
    }
    id
}

/// Wait until a snapshot shows `avatar` holding `want`.
async fn holding(c: &mut Client, avatar: u64, want: Option<i64>) {
    next(c, |m| match m {
        NetMessage::WorldSnapshot { avatars, .. } => avatars
            .iter()
            .find(|a| a.id == avatar)
            .filter(|a| a.holding == want)
            .map(|_| ()),
        _ => None,
    })
    .await;
}

/// The code of the refusal answering `request_id`.
async fn refusal(c: &mut Client, request_id: u32) -> u32 {
    next(c, |m| match m {
        NetMessage::ServerError {
            request_id: r,
            code,
            ..
        } if r == request_id => Some(code),
        _ => None,
    })
    .await
}

async fn grab(c: &mut Client, request_id: u32, prim_id: i64) {
    send(
        c,
        &NetMessage::GrabPrim {
            request_id,
            prim_id,
        },
    )
    .await;
}

async fn release(c: &mut Client, throw: bool) {
    send(
        c,
        &NetMessage::ReleasePrim {
            request_id: 0,
            throw,
        },
    )
    .await;
}

/// The ball once it has come to rest.
async fn rested(c: &mut Client, id: i64) -> PrimBody {
    next(c, |m| ball_body(m, id).filter(|b| b.resting)).await
}

const EAST: f32 = std::f32::consts::FRAC_PI_2;

#[tokio::test]
async fn a_ball_is_picked_up_carried_and_thrown() {
    let (_dir, s) = world().await;
    let (mut a, alice) = connect(&s, "alice").await;
    let (mut b, _) = connect(&s, "bob").await;
    place(&mut a, at(PITCH, 0.0, 4.0), 0.0).await;
    place(&mut b, at(PITCH, 0.0, 4.0), 0.0).await;

    let id = make(
        &mut a,
        "ball",
        "sphere",
        Vec3::splat(0.22),
        0.5,
        PITCH,
        true,
    )
    .await;
    let rest = rested(&mut b, id).await;

    // A metre west of it, facing it, and pick it up.
    place(&mut a, at(rest.geo, -1.0, 0.0), EAST).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    grab(&mut a, 5, id).await;
    holding(&mut b, alice, Some(id)).await;

    // Held in the hands, at chest height, in front of her.
    let held = next(&mut b, |m| {
        ball_body(m, id).filter(|b| (b.altitude - 1.1).abs() < 0.1)
    })
    .await;
    assert!(
        east_of(rest.geo, held.geo) > -0.7,
        "held east of her feet, in front: {} m",
        east_of(rest.geo, held.geo)
    );

    // She walks east for a second and the ball comes with her.
    send(
        &mut a,
        &NetMessage::ClientIntent {
            request_id: 0,
            move_x: 0.5,
            move_z: 0.0,
            display_yaw: EAST,
            flying: false,
            fly_up: false,
            fly_down: false,
            jump: false,
            emote: None,
            expression: None,
            seated: None,
            geo: None,
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    send(&mut a, &stand(PITCH, EAST, None)).await;
    let carried = next(&mut b, |m| {
        ball_body(m, id).filter(|b| east_of(rest.geo, b.geo) > 1.5)
    })
    .await;
    assert!(
        (carried.altitude - 1.1).abs() < 0.15,
        "carried, not dropped: {} m up",
        carried.altitude
    );

    // And throws it: off east, up, and down again further on.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let from = next(&mut b, |m| ball_body(m, id)).await.geo;
    release(&mut a, true).await;
    let thrown = next(&mut b, |m| ball_body(m, id).filter(|b| b.velocity.x > 5.0)).await;
    assert!(
        thrown.velocity.z.abs() < 1.0,
        "thrown east, went {:?}",
        thrown.velocity
    );
    holding(&mut b, alice, None).await;
    // A thrown ball rolls on for a while after it lands, so what is waited
    // for is the ball on the ground well out from the hands, not at rest.
    let landed = next(&mut b, |m| {
        ball_body(m, id).filter(|b| b.altitude < 0.2 && east_of(from, b.geo) > 3.0)
    })
    .await;
    assert!(
        landed.velocity.x >= 0.0,
        "still going east: {:?}",
        landed.velocity
    );
}

#[tokio::test]
async fn the_sim_refuses_what_may_not_be_picked_up() {
    let (_dir, s) = world().await;
    let (mut a, alice) = connect(&s, "alice").await;
    let (mut b, _) = connect(&s, "bob").await;
    // Yaw 0 faces south, towards the ball.
    place(&mut a, at(PITCH, 0.0, 1.0), 0.0).await;
    place(&mut b, at(PITCH, 0.0, 12.0), 0.0).await;

    let ball = make(
        &mut a,
        "ball",
        "sphere",
        Vec3::splat(0.22),
        0.11,
        PITCH,
        true,
    )
    .await;
    let big = make(
        &mut a,
        "crate",
        "box",
        Vec3::ONE,
        0.5,
        at(PITCH, 1.6, 0.0),
        true,
    )
    .await;
    let fixed = make(
        &mut a,
        "cube",
        "box",
        Vec3::splat(0.3),
        0.15,
        at(PITCH, -1.6, 0.0),
        false,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Bob is twelve metres off: out of reach.
    grab(&mut b, 10, ball).await;
    assert_eq!(refusal(&mut b, 10).await, 422, "out of reach");

    // Alice is a metre from it: hers.
    grab(&mut a, 11, ball).await;
    holding(&mut a, alice, Some(ball)).await;

    // Bob comes over, and may not take it out of her hands.
    place(&mut b, at(PITCH, 0.0, -1.0), 0.0).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    grab(&mut b, 12, ball).await;
    assert_eq!(refusal(&mut b, 12).await, 409, "held by somebody else");

    // One thing at a time.
    grab(&mut a, 13, fixed).await;
    assert_eq!(refusal(&mut a, 13).await, 409, "hands full");

    // Put down, it is on the ground in front of her and her hands are empty.
    release(&mut a, false).await;
    holding(&mut a, alice, None).await;
    let down = rested(&mut b, ball).await;
    assert!(down.altitude < 0.2, "set down at {} m", down.altitude);

    // A crate a metre on a side is 100 kg.
    grab(&mut a, 14, big).await;
    assert_eq!(refusal(&mut a, 14).await, 422, "too big");
    // A cube that is not physical is fixed where it was built.
    grab(&mut a, 15, fixed).await;
    assert_eq!(refusal(&mut a, 15).await, 422, "not physical");
    // And nothing at all.
    grab(&mut a, 16, 987_654).await;
    assert_eq!(refusal(&mut a, 16).await, 404, "no such prim");
}

#[tokio::test]
async fn leaving_a_teleport_and_switching_off_each_let_go() {
    let (_dir, s) = world().await;
    let (mut a, alice) = connect(&s, "alice").await;
    let (mut b, bob) = connect(&s, "bob").await;
    let (mut c, _) = connect(&s, "carol").await;
    // Yaw 0 faces south, towards the ball.
    place(&mut a, at(PITCH, 0.0, 1.0), 0.0).await;
    place(&mut b, at(PITCH, 0.0, -1.0), 0.0).await;
    place(&mut c, at(PITCH, 0.0, 5.0), 0.0).await;
    let ball = make(
        &mut a,
        "ball",
        "sphere",
        Vec3::splat(0.22),
        0.11,
        PITCH,
        true,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Bob picks it up, and leaves: it falls and is free for anyone.
    grab(&mut b, 20, ball).await;
    holding(&mut c, bob, Some(ball)).await;
    next(&mut c, |m| {
        ball_body(m, ball).filter(|b| (b.altitude - 1.1).abs() < 0.1)
    })
    .await;
    drop(b);
    let fell = rested(&mut c, ball).await;
    assert!(fell.altitude < 0.2, "fell to {} m", fell.altitude);

    // Alice walks over to where it fell and picks it up…
    place(&mut a, at(fell.geo, -1.0, 0.0), EAST).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    grab(&mut a, 21, ball).await;
    holding(&mut c, alice, Some(ball)).await;
    // …and a teleport leaves it behind.
    place(&mut a, at(PITCH, 0.0, 8.0), 0.0).await;
    holding(&mut c, alice, None).await;
    let left = rested(&mut c, ball).await;
    assert!(
        east_of(fell.geo, left.geo).abs() < 2.0,
        "left where she was, not carried 8 m: {} m",
        east_of(fell.geo, left.geo)
    );

    // Back, picked up again, and switched off by its owner: let go.
    place(&mut a, at(left.geo, -1.0, 0.0), EAST).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    grab(&mut a, 22, ball).await;
    holding(&mut c, alice, Some(ball)).await;
    send(
        &mut a,
        &NetMessage::SetPrimPhysical {
            request_id: 23,
            prim_id: ball,
            physical: false,
        },
    )
    .await;
    holding(&mut c, alice, None).await;
}
