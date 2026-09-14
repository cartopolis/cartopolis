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
