//! End-to-end chat over loopback TCP (ADR-028): global delivery to another
//! connected client, and scrollback replay to a client that connects later.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use vibe_core::{
    decode_app_frame, encode_app_frame, AuthCredential, ChatScope, NetMessage, PROTOCOL_VERSION,
};
use vibe_sim::{ensure_account, serve, SimConfig};

type Client = Framed<TcpStream, LengthDelimitedCodec>;

async fn connect_auth(addr: &str) -> Client {
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
            username: "dev".into(),
            password: "dev".into(),
        },
    })
    .unwrap();
    c.send(Bytes::from(hello)).await.unwrap();
    // First frame back is the ack.
    let ack = decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap();
    assert!(matches!(ack, NetMessage::ServerHelloAck { .. }));
    c
}

/// Read frames (skipping world snapshots etc.) until a ChatMessage arrives.
async fn recv_chat(c: &mut Client) -> (String, String) {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(3), c.next())
            .await
            .expect("timed out waiting for chat")
            .unwrap()
            .unwrap();
        if let NetMessage::ChatMessage { channel, body, .. } = decode_app_frame(&frame).unwrap() {
            return (channel, body);
        }
    }
}

#[tokio::test]
async fn global_delivery_and_scrollback() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chat.db").to_str().unwrap().to_string();
    {
        let conn = vibe_sim::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, "dev", "dev").unwrap();
    }
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    let addr = handle.local_addr.to_string();

    // Two clients connected; A shouts globally, B receives it.
    let mut a = connect_auth(&addr).await;
    let mut b = connect_auth(&addr).await;
    let send = encode_app_frame(&NetMessage::ChatSend {
        request_id: 1,
        scope: ChatScope::Global,
        body: "hello world".into(),
    })
    .unwrap();
    a.send(Bytes::from(send)).await.unwrap();

    let (channel, body) = recv_chat(&mut b).await;
    assert_eq!(channel, "geo:");
    assert_eq!(body, "hello world");

    // A client connecting afterward gets the message from scrollback replay.
    let mut c = connect_auth(&addr).await;
    let (channel, body) = recv_chat(&mut c).await;
    assert_eq!(channel, "geo:");
    assert_eq!(body, "hello world");
}
