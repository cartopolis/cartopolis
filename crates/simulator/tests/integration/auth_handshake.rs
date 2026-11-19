//! End-to-end auth handshake over real loopback TCP (ADR-016) — the path solo
//! mode and `--connect` both use. Verifies password login issues a session token,
//! that token reconnects, and a bad password is rejected.

use std::sync::Arc;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, NetMessage, PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, serve, SimConfig};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Client = Framed<TcpStream, LengthDelimitedCodec>;

async fn connect(addr: &str) -> Client {
    let stream = TcpStream::connect(addr).await.unwrap();
    Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(32 << 20)
            .little_endian()
            .new_codec(),
    )
}

async fn hello(client: &mut Client, auth: AuthCredential) -> NetMessage {
    let frame = encode_app_frame(&NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth,
    })
    .unwrap();
    client.send(Bytes::from(frame)).await.unwrap();
    let reply = client.next().await.unwrap().unwrap();
    decode_app_frame(&reply).unwrap()
}

#[tokio::test]
async fn password_login_then_token_reconnect_then_reject() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("auth.db").to_str().unwrap().to_string();

    // Provision the account exactly as solo mode does, then serve on an
    // ephemeral loopback port.
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, "dev", "dev").unwrap();
    }
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    let addr = handle.local_addr.to_string();

    // 1. Password login → ServerHelloAck carrying a session token.
    let mut c1 = connect(&addr).await;
    let token = match hello(
        &mut c1,
        AuthCredential::Password {
            username: "dev".into(),
            password: "dev".into(),
        },
    )
    .await
    {
        NetMessage::ServerHelloAck {
            session_token,
            username,
            ..
        } => {
            assert!(!session_token.is_empty(), "token must be issued");
            assert_eq!(username, "dev", "server echoes the authoritative username");
            session_token
        }
        other => panic!("expected ServerHelloAck, got {other:?}"),
    };

    // 2. Reconnect with the issued token.
    let mut c2 = connect(&addr).await;
    assert!(
        matches!(
            hello(&mut c2, AuthCredential::Token(token)).await,
            NetMessage::ServerHelloAck { .. }
        ),
        "token should authenticate"
    );

    // 3. Wrong password is rejected with a 401 ServerError.
    let mut c3 = connect(&addr).await;
    assert!(
        matches!(
            hello(
                &mut c3,
                AuthCredential::Password {
                    username: "dev".into(),
                    password: "nope".into(),
                },
            )
            .await,
            NetMessage::ServerError { code: 401, .. }
        ),
        "bad password should be rejected"
    );
}

/// Where the avatar was left comes back in the next sign-in's handshake, so the
/// client can put it there (`ServerHelloAck::last_place`) — on any device, and
/// not on whatever its map happens to be anchored to.
#[tokio::test]
async fn the_next_sign_in_is_told_where_the_avatar_was_left() {
    use cartopolis_core::GeoAnchor;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("place.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, "dev", "dev").unwrap();
    }
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    let addr = handle.local_addr.to_string();
    let password = || AuthCredential::Password {
        username: "dev".into(),
        password: "dev".into(),
    };
    let place_in = |ack: NetMessage| match ack {
        NetMessage::ServerHelloAck { last_place, .. } => last_place,
        other => panic!("expected ServerHelloAck, got {other:?}"),
    };

    // A new account has never stood anywhere.
    let mut first = connect(&addr).await;
    assert_eq!(place_in(hello(&mut first, password()).await), None);
    let left = GeoAnchor {
        latitude: 53.2163,
        longitude: 6.5711,
    };
    let intent = encode_app_frame(&NetMessage::ClientIntent {
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
        geo: Some(left),
    })
    .unwrap();
    first.send(Bytes::from(intent)).await.unwrap();
    // Leave once the sim has had time to take the intent. Each probe below is a
    // session of its own that never says where it is, which must leave the
    // remembered place alone.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    drop(first);

    // The guard writes the place off the connection's task, so give it a beat.
    let mut told = None;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut again = connect(&addr).await;
        told = place_in(hello(&mut again, password()).await);
        if told.is_some() {
            break;
        }
    }
    assert_eq!(told, Some(left));
}

/// An app older than the server is answered, not just dropped: the refusal
/// comes in the server's own wire version, which the old app cannot decode and
/// so reads as "too old, update" (`platform::update::mark_too_old`). Closing
/// without a word left it at "connection reset", with no way to tell why.
#[tokio::test]
async fn an_older_app_is_told_the_server_is_newer() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("old.db").to_str().unwrap().to_string();
    cartopolis_simulator::db::open_and_migrate(&db).unwrap();
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    let mut client = connect(&handle.local_addr.to_string()).await;

    // An old build's hello: its own version in the header and the hello.
    let mut frame = encode_app_frame(&NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION - 1,
        auth: AuthCredential::Token("x".into()),
    })
    .unwrap();
    frame[0..2].copy_from_slice(&(PROTOCOL_VERSION - 1).to_le_bytes());
    client.send(Bytes::from(frame)).await.unwrap();

    let reply = client
        .next()
        .await
        .expect("the server answers before it closes")
        .unwrap();
    // What the old app sees is a version newer than its own…
    let header = u16::from_le_bytes(reply[0..2].try_into().unwrap());
    assert!(header > PROTOCOL_VERSION - 1);
    // …and what this build reads is the refusal itself.
    assert!(matches!(
        decode_app_frame(&reply),
        Ok(NetMessage::ServerError { code: 1, .. })
    ));
}
