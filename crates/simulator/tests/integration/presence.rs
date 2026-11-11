//! An idle client stops being sent world snapshots and stays reachable
//! (protocol 18, `NetMessage::SetPresence`).
//!
//! The two halves are one feature and neither is worth much alone. Dropping the
//! snapshots is what makes a backgrounded phone cheap enough to leave connected;
//! still delivering chat is the entire reason to leave it connected. A test that
//! only checked the first would pass just as happily on a server that had hung up.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, ChatScope, GeoAnchor, NetMessage,
    PROTOCOL_VERSION,
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

/// Say where the avatar stands: nothing is within reach of an avatar that is
/// nowhere, so a connection that never does receives no snapshots and no
/// things, whatever its presence says (protocol 47).
async fn stand(c: &mut Client, lat: f64, lng: f64) {
    let frame = encode_app_frame(&NetMessage::ClientIntent {
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
        geo: Some(GeoAnchor {
            latitude: lat,
            longitude: lng,
        }),
    })
    .unwrap();
    c.send(Bytes::from(frame)).await.unwrap();
}

/// Groningen's Grote Markt, where these avatars stand.
const LAT: f64 = 53.2194;
const LNG: f64 = 6.5665;

async fn set_presence(c: &mut Client, active: bool) {
    let frame = encode_app_frame(&NetMessage::SetPresence {
        request_id: 0,
        active,
    })
    .unwrap();
    c.send(Bytes::from(frame)).await.unwrap();
}

/// Count snapshots arriving over a window, returning any chat seen alongside.
///
/// Reads to a deadline rather than for a fixed number of frames: the assertion
/// is about what *does not* arrive, so the test has to let the clock run and
/// then look at what it caught.
async fn drain(c: &mut Client, window: Duration) -> (usize, Vec<String>) {
    let deadline = tokio::time::Instant::now() + window;
    let mut snapshots = 0;
    let mut chat = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return (snapshots, chat);
        }
        match tokio::time::timeout(left, c.next()).await {
            Ok(Some(Ok(frame))) => match decode_app_frame(&frame).unwrap() {
                NetMessage::WorldSnapshot { .. } => snapshots += 1,
                NetMessage::ChatMessage { body, .. } => chat.push(body),
                _ => {}
            },
            // Closed, errored, or the window expired.
            Ok(_) | Err(_) => return (snapshots, chat),
        }
    }
}

/// The handle must be **returned and held**, not dropped at the end of this
/// function: dropping it shuts the server down, and every later call then fails
/// with a bare `ConnectionReset` at the handshake that reads like a protocol bug.
async fn boot() -> (tempfile::TempDir, cartopolis_simulator::SimHandle, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("presence.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, "dev", "dev").unwrap();
        ensure_account(&conn, "other", "dev").unwrap();
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

/// The window has to span several ticks or "no snapshots" proves nothing —
/// at the default tick rate this is tens of them.
const WINDOW: Duration = Duration::from_millis(600);

#[tokio::test]
async fn an_idle_client_is_sent_no_snapshots_and_still_hears_chat() {
    let (_dir, _handle, addr) = boot().await;
    let mut idle = connect_auth(&addr, "dev").await;
    let mut talker = connect_auth(&addr, "other").await;
    stand(&mut idle, LAT, LNG).await;

    // Control: while active, snapshots arrive. Without this the test would pass
    // against a server that never sent any.
    let (before, _) = drain(&mut idle, WINDOW).await;
    assert!(
        before > 0,
        "expected snapshots while active, got none — the rest of this test would be vacuous"
    );

    set_presence(&mut idle, false).await;
    // The flag is applied when the message is read, and a snapshot already in
    // flight is still on its way, so let the switch settle before measuring.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let _ = drain(&mut idle, Duration::from_millis(100)).await;

    let send = encode_app_frame(&NetMessage::ChatSend {
        request_id: 1,
        scope: ChatScope::Global,
        body: "are you there".into(),
    })
    .unwrap();
    talker.send(Bytes::from(send)).await.unwrap();

    let (snapshots, chat) = drain(&mut idle, WINDOW).await;
    assert_eq!(snapshots, 0, "an idle client was sent world snapshots");
    assert!(
        chat.iter().any(|b| b == "are you there"),
        "an idle client stopped hearing chat, which is the whole reason to stay connected: {chat:?}"
    );
}

#[tokio::test]
async fn coming_back_resumes_snapshots() {
    let (_dir, _handle, addr) = boot().await;
    let mut c = connect_auth(&addr, "dev").await;
    stand(&mut c, LAT, LNG).await;

    set_presence(&mut c, false).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let _ = drain(&mut c, Duration::from_millis(100)).await;
    let (idle, _) = drain(&mut c, WINDOW).await;
    assert_eq!(idle, 0);

    set_presence(&mut c, true).await;
    let (resumed, _) = drain(&mut c, WINDOW).await;
    assert!(
        resumed > 0,
        "a client that came back was never sent another snapshot — it would sit \
         on a frozen world with nothing to ask"
    );
}

/// Things come within reach and leave it (protocol 47).
///
/// The handshake's snapshot is empty: nothing is within reach of an avatar that
/// is nowhere, and nothing arrives until its client says where it stands. Then
/// the bench by the place it stands arrives as a `PrimUpsert`, a teleport to
/// another city takes it away with a `PrimOutOfReach` — not a `PrimRemoved`,
/// the bench still exists — and coming back brings it again. A unit test on
/// `SimWorld` cannot catch any of this, because all of it is what one
/// connection is sent and in what order.
#[tokio::test]
async fn things_come_within_reach_and_leave_it() {
    let (_dir, _handle, addr, bench) = boot_with_a_prim().await;
    let mut c = connect_auth(&addr, "dev").await;

    let first = next_of(&mut c, |m| match m {
        NetMessage::WorldSnapshot { avatars, .. } => Some(avatars.len()),
        _ => None,
    })
    .await;
    assert_eq!(first, 0, "the handshake's snapshot is empty");
    // The ticks still come, empty: nothing is within reach of nowhere, and the
    // snapshot is also the heartbeat a resumed link is judged by.
    let before = collect(&mut c, Duration::from_millis(300)).await;
    assert!(
        before
            .iter()
            .any(|m| matches!(m, NetMessage::WorldSnapshot { .. })),
        "the ticks keep coming to an avatar that is nowhere"
    );
    assert!(
        !before.iter().any(|m| matches!(
            m,
            NetMessage::WorldSnapshot { avatars, .. } if !avatars.is_empty()
        ) || matches!(m, NetMessage::PrimUpsert { .. })),
        "nothing is within reach of an avatar that is nowhere: {before:?}"
    );

    stand(&mut c, LAT, LNG).await;
    let arrived = next_of(&mut c, |m| match m {
        NetMessage::PrimUpsert { prim } => Some(prim.id),
        _ => None,
    })
    .await;
    assert_eq!(
        arrived, bench,
        "the bench by the Grote Markt comes within reach"
    );
    let shown = next_of(&mut c, |m| match m {
        NetMessage::WorldSnapshot { avatars, .. } if !avatars.is_empty() => Some(avatars),
        _ => None,
    })
    .await;
    assert_eq!(shown.len(), 1, "the avatar sees itself: {shown:?}");
    assert!(
        (shown[0].geo.latitude - LAT).abs() < 1e-9 && (shown[0].geo.longitude - LNG).abs() < 1e-9
    );

    // Amsterdam: well beyond any reach.
    teleport(&mut c, 52.3731, 4.8926).await;
    let gone = next_of(&mut c, |m| match m {
        NetMessage::PrimOutOfReach { id } => Some(id),
        NetMessage::PrimRemoved { .. } => panic!("a thing out of reach is not a deleted thing"),
        _ => None,
    })
    .await;
    assert_eq!(gone, bench);

    teleport(&mut c, LAT, LNG).await;
    let back = next_of(&mut c, |m| match m {
        NetMessage::PrimUpsert { prim } => Some(prim.id),
        _ => None,
    })
    .await;
    assert_eq!(back, bench, "coming back brings the bench again");
}

async fn teleport(c: &mut Client, lat: f64, lng: f64) {
    let frame = encode_app_frame(&NetMessage::Teleport {
        request_id: 0,
        geo: GeoAnchor {
            latitude: lat,
            longitude: lng,
        },
        alt_m: 0.0,
    })
    .unwrap();
    c.send(Bytes::from(frame)).await.unwrap();
}

/// The first message `f` accepts, within five seconds.
async fn next_of<T>(c: &mut Client, mut f: impl FnMut(NetMessage) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = tokio::time::timeout_at(deadline, c.next())
            .await
            .expect("a matching message within five seconds")
            .unwrap()
            .unwrap();
        if let Some(v) = f(decode_app_frame(&frame).unwrap()) {
            return v;
        }
    }
}

/// Everything that arrives over `window`.
async fn collect(c: &mut Client, window: Duration) -> Vec<NetMessage> {
    let deadline = tokio::time::Instant::now() + window;
    let mut out = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return out;
        }
        match tokio::time::timeout(left, c.next()).await {
            Ok(Some(Ok(frame))) => out.push(decode_app_frame(&frame).unwrap()),
            Ok(_) | Err(_) => return out,
        }
    }
}

/// [`boot`] with one prim on the Grote Markt, and its id.
async fn boot_with_a_prim() -> (
    tempfile::TempDir,
    cartopolis_simulator::SimHandle,
    String,
    i64,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("handshake.db")
        .to_str()
        .unwrap()
        .to_string();
    let bench = {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, "dev", "dev").unwrap();
        cartopolis_simulator::db::insert_prim(
            &conn,
            glam::Vec3::ZERO,
            "box",
            GeoAnchor {
                latitude: LAT,
                longitude: LNG,
            },
            None,
            None,
        )
        .unwrap()
        .id
    };
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    let addr = handle.local_addr.to_string();
    (dir, handle, addr, bench)
}

/// A client that never sends the message behaves exactly as it did before
/// protocol 18. This is the compatibility guarantee that let the version bump
/// land without touching any other client.
#[tokio::test]
async fn a_client_that_never_mentions_presence_is_active() {
    let (_dir, _handle, addr) = boot().await;
    let mut c = connect_auth(&addr, "dev").await;
    stand(&mut c, LAT, LNG).await;
    let (snapshots, _) = drain(&mut c, WINDOW).await;
    assert!(snapshots > 0);
}
