//! Greeting another avatar (protocol 49), end to end over the wire: an offer
//! within reach, accepted, starts the handshake on both avatars in one
//! snapshot with one sequence and turns them to each other; offers to oneself,
//! out of reach, between people one of whom blocked the other, after they
//! lapse and after the offerer walked off are refused.

use std::time::Duration;

use cartopolis_core::greet::{Greeting, OFFER_SECS};
use cartopolis_core::{
    decode_app_frame, AuthCredential, AvatarStateDto, Emote, GeoAnchor, NetMessage,
    PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, SimHandle};
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use super::physical_prims::{next, send, server, stand, Client, PITCH};

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
    let db = dir.path().join("greet.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        for name in ["alice", "bob", "carol"] {
            ensure_account(&conn, name, "pw").unwrap();
        }
    }
    let s = server(&db).await;
    (dir, s)
}

/// `PITCH` moved `east` metres.
fn east(m: f64) -> GeoAnchor {
    let (_, m_lng) = cartopolis_core::degree_scale_m(PITCH.latitude);
    GeoAnchor {
        latitude: PITCH.latitude,
        longitude: PITCH.longitude + m / m_lng,
    }
}

/// `b` from `a` in metres (east, south).
fn offset(a: GeoAnchor, b: GeoAnchor) -> (f32, f32) {
    let (m_lat, m_lng) = cartopolis_core::degree_scale_m(a.latitude);
    (
        ((b.longitude - a.longitude) * m_lng) as f32,
        ((a.latitude - b.latitude) * m_lat) as f32,
    )
}

/// Put an avatar `m` metres east of the pitch, facing north.
async fn place(c: &mut Client, m: f64) {
    // Past the intent rate limit (10 ms), so the standing intent is not
    // dropped behind a walking one sent just before.
    tokio::time::sleep(Duration::from_millis(30)).await;
    let geo = east(m);
    send(
        c,
        &NetMessage::Teleport {
            request_id: 0,
            geo,
            alt_m: 0.0,
        },
    )
    .await;
    send(c, &stand(geo, std::f32::consts::PI, None)).await;
}

async fn offer(c: &mut Client, request_id: u32, to: u64, greeting: Greeting) {
    send(
        c,
        &NetMessage::OfferGreeting {
            request_id,
            to,
            greeting,
        },
    )
    .await;
}

async fn accept(c: &mut Client, request_id: u32, from: u64) {
    send(c, &NetMessage::AcceptGreeting { request_id, from }).await;
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

/// The offer `c` is told of from `from`: its greeting, `None` when withdrawn.
async fn offered(c: &mut Client, from: u64) -> Option<Greeting> {
    next(c, |m| match m {
        NetMessage::GreetingOffered {
            from: f, greeting, ..
        } if f == from => Some(greeting),
        _ => None,
    })
    .await
}

/// Let the placements land before the next request reads them.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn an_accepted_greeting_starts_both_together_facing_each_other() {
    let (_dir, s) = world().await;
    let (mut a, alice) = connect(&s, "alice").await;
    let (mut b, bob) = connect(&s, "bob").await;
    let (mut c, _) = connect(&s, "carol").await;
    place(&mut a, 0.0).await;
    place(&mut b, 2.0).await;
    place(&mut c, 5.0).await;
    settle().await;

    offer(&mut a, 1, bob, Greeting::Handshake).await;
    assert_eq!(offered(&mut b, alice).await, Some(Greeting::Handshake));
    accept(&mut b, 2, alice).await;

    // Each is told, with the way to face.
    let started = |me: u64| {
        move |m| match m {
            NetMessage::GreetingStarted {
                greeting,
                partner,
                yaw,
            } if partner == me => Some((greeting, yaw)),
            _ => None,
        }
    };
    let (ga, yaw_a) = next(&mut a, started(bob)).await;
    let (gb, yaw_b) = next(&mut b, started(alice)).await;
    assert_eq!((ga, gb), (Greeting::Handshake, Greeting::Handshake));

    // Carol sees both start in one snapshot, with one sequence, turned to
    // each other at the clip's spacing.
    let (sa, sb): (AvatarStateDto, AvatarStateDto) = next(&mut c, |m| match m {
        NetMessage::WorldSnapshot { avatars, .. } => {
            let find = |id: u64| avatars.iter().find(|s| s.id == id).cloned();
            let (sa, sb) = (find(alice)?, find(bob)?);
            (sa.emote.is_some() || sb.emote.is_some()).then_some((sa, sb))
        }
        _ => None,
    })
    .await;
    let (ea, eb) = (sa.emote.unwrap(), sb.emote.unwrap());
    assert_eq!((ea.emote, eb.emote), (Emote::Handshake, Emote::Handshake));
    assert_eq!(ea.sequence, eb.sequence, "one sequence for the pair");
    assert_eq!((sa.yaw, sb.yaw), (yaw_a, yaw_b));
    let (dx, dz) = offset(sa.geo, sb.geo);
    let [px, pz] = Greeting::Handshake.partner_at();
    assert!(
        (dx.hypot(dz) - px.hypot(pz)).abs() < 0.02,
        "stood {} m apart",
        dx.hypot(dz)
    );
    // Each faces the other, give or take the offset to the left that lets
    // right hands meet.
    for (yaw, to) in [(sa.yaw, (dx, dz)), (sb.yaw, (-dx, -dz))] {
        let facing = (yaw.sin(), yaw.cos());
        let cos = (facing.0 * to.0 + facing.1 * to.1) / to.0.hypot(to.1);
        assert!(cos > 40f32.to_radians().cos(), "facing {cos} of the way");
    }
    let middle = offset(PITCH, sa.geo).0 + dx * 0.5;
    assert!((middle - 1.0).abs() < 0.05, "about their middle: {middle}");
}

#[tokio::test]
async fn greetings_that_cannot_happen_are_refused() {
    let (_dir, s) = world().await;
    let (mut a, alice) = connect(&s, "alice").await;
    let (mut b, bob) = connect(&s, "bob").await;
    let (mut c, carol) = connect(&s, "carol").await;
    place(&mut a, 0.0).await;
    place(&mut b, 2.0).await;
    place(&mut c, 6.0).await;
    settle().await;

    // To oneself, and to somebody across the square.
    offer(&mut a, 1, alice, Greeting::HighFive).await;
    assert_eq!(refusal(&mut a, 1).await, 422);
    offer(&mut a, 2, carol, Greeting::HighFive).await;
    assert_eq!(refusal(&mut a, 2).await, 422);
    // Accepting what nobody offered.
    accept(&mut c, 3, alice).await;
    assert_eq!(refusal(&mut c, 3).await, 404);

    // Bob blocks alice: neither may greet the other, and bob hears nothing.
    send(
        &mut b,
        &NetMessage::SetBlocked {
            request_id: 0,
            username: "alice".into(),
            blocked: true,
        },
    )
    .await;
    next(&mut b, |m| {
        matches!(m, NetMessage::BlockedList { .. }).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    offer(&mut a, 4, bob, Greeting::Handshake).await;
    assert_eq!(refusal(&mut a, 4).await, 403);
    offer(&mut b, 5, alice, Greeting::Handshake).await;
    assert_eq!(refusal(&mut b, 5).await, 403);
}

#[tokio::test]
async fn an_offer_lapses_and_walking_off_withdraws_it() {
    let (_dir, s) = world().await;
    let (mut a, alice) = connect(&s, "alice").await;
    let (mut b, bob) = connect(&s, "bob").await;
    place(&mut a, 0.0).await;
    place(&mut b, 1.5).await;
    settle().await;

    // Walking off withdraws it, and bob is told.
    offer(&mut a, 1, bob, Greeting::HighFive).await;
    assert_eq!(offered(&mut b, alice).await, Some(Greeting::HighFive));
    send(
        &mut a,
        &NetMessage::ClientIntent {
            request_id: 0,
            move_x: 0.0,
            move_z: -0.3,
            display_yaw: std::f32::consts::PI,
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
    assert_eq!(offered(&mut b, alice).await, None, "withdrawn");
    accept(&mut b, 2, alice).await;
    assert_eq!(refusal(&mut b, 2).await, 404);

    // Back, and an offer left unanswered lapses.
    place(&mut a, 0.0).await;
    settle().await;
    offer(&mut a, 3, bob, Greeting::Handshake).await;
    assert_eq!(offered(&mut b, alice).await, Some(Greeting::Handshake));
    tokio::time::sleep(Duration::from_secs_f32(OFFER_SECS + 0.6)).await;
    accept(&mut b, 4, alice).await;
    assert_eq!(refusal(&mut b, 4).await, 404, "lapsed");
}
