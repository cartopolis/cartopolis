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
    decode_app_frame, encode_app_frame, AuthCredential, ChatScope, NetMessage, PROTOCOL_VERSION,
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

/// The world arrives at the handshake, and the repeating snapshots do not carry
/// it.
///
/// Both halves matter and neither is safe alone. The client builds its prim
/// entities from the **first** `WorldSnapshot` it sees and ignores the list in
/// every one after it, so if the handshake stopped sending a full one the world
/// would simply never appear — and if the tick loop went back to sending the full
/// one, the server would be serialising and fanning out the whole world twenty
/// times a second into a discard. A unit test on `SimWorld` cannot catch either,
/// because both are about what actually reaches a socket and in what order.
#[tokio::test]
async fn the_world_arrives_once_at_the_handshake() {
    let (_dir, _handle, addr) = boot_with_a_prim().await;
    let mut c = connect_auth(&addr, "dev").await;

    // Read to the first snapshot. The frames before it (capacity, block list,
    // chat scrollback) are not what this test is about, but their presence is
    // exactly why it reads rather than assuming a position.
    let first = loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), c.next())
            .await
            .expect("a snapshot within five seconds")
            .unwrap()
            .unwrap();
        if let NetMessage::WorldSnapshot { prims, .. } = decode_app_frame(&frame).unwrap() {
            break prims;
        }
    };
    assert_eq!(
        first.len(),
        1,
        "the first snapshot must carry the world, or the client never draws one"
    );

    // Every later one, over a window spanning many ticks.
    let deadline = tokio::time::Instant::now() + WINDOW;
    let mut later = 0;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(Some(Ok(frame))) = tokio::time::timeout(left, c.next()).await else {
            break;
        };
        if let NetMessage::WorldSnapshot { prims, .. } = decode_app_frame(&frame).unwrap() {
            later += 1;
            assert!(
                prims.is_empty(),
                "a repeating snapshot carried the prim list; prims travel as PrimUpsert"
            );
        }
    }
    assert!(
        later > 0,
        "no repeating snapshots arrived at all, so the assertion above proved nothing"
    );
}

/// [`boot`] with one prim in the world, so "the world arrived" is distinguishable
/// from "the list was empty anyway".
async fn boot_with_a_prim() -> (tempfile::TempDir, cartopolis_simulator::SimHandle, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("handshake.db")
        .to_str()
        .unwrap()
        .to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, "dev", "dev").unwrap();
        conn.execute(
            "INSERT INTO prims (name, shape) VALUES ('Bench', 'box')",
            [],
        )
        .unwrap();
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

/// A client that never sends the message behaves exactly as it did before
/// protocol 18. This is the compatibility guarantee that let the version bump
/// land without touching any other client.
#[tokio::test]
async fn a_client_that_never_mentions_presence_is_active() {
    let (_dir, _handle, addr) = boot().await;
    let mut c = connect_auth(&addr, "dev").await;
    let (snapshots, _) = drain(&mut c, WINDOW).await;
    assert!(snapshots > 0);
}
