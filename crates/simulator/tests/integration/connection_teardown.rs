//! A connection that dies mid-session must take its avatar with it.
//!
//! `handle_connection` ends with the teardown that unregisters the chat inbox and
//! removes the avatar from `SimWorld`. Every fallible step in the message loop
//! used to reach that teardown only by falling out of the loop — except the
//! incoming-frame arm, where a bare `?` returned straight out of the function and
//! skipped it. One malformed frame from an authenticated client therefore leaked
//! its avatar, and since `SimWorld::snapshot` replicates every avatar to every
//! client, the leak showed up on everyone else's screen as a motionless ghost for
//! the life of the process.

use std::sync::Arc;
use std::time::Duration;

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

async fn login(addr: &str) -> Client {
    let mut client = connect(addr).await;
    let frame = encode_app_frame(&NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth: AuthCredential::Password {
            username: "dev".into(),
            password: "dev".into(),
        },
    })
    .unwrap();
    client.send(Bytes::from(frame)).await.unwrap();
    let reply = client.next().await.unwrap().unwrap();
    match decode_app_frame(&reply).unwrap() {
        NetMessage::ServerHelloAck { .. } => client,
        other => panic!("expected ServerHelloAck, got {other:?}"),
    }
}

/// A frame the server accepts as far as the header and cannot decode as a body:
/// a well-formed 8-byte header for `ClientIntent` followed by no payload at all.
/// This is the shape a truncated write produces.
fn undecodable_frame() -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes()); // MessageKind::ClientIntent
    out.extend_from_slice(&0u32.to_le_bytes()); // request_id
    out
}

/// Block until a snapshot reports `want` avatars, or give up.
async fn wait_for_avatar_count(client: &mut Client, want: usize) -> Result<(), usize> {
    let mut last = usize::MAX;
    let deadline = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let Some(Ok(bytes)) = client.next().await else {
                return;
            };
            if let Ok(NetMessage::WorldSnapshot { avatars, .. }) = decode_app_frame(&bytes) {
                last = avatars.len();
                if last == want {
                    return;
                }
            }
        }
    })
    .await;
    if deadline.is_ok() && last == want {
        Ok(())
    } else {
        Err(last)
    }
}

#[tokio::test]
async fn a_malformed_frame_removes_the_sender_s_avatar() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("teardown.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, "dev", "dev").unwrap();
    }
    let handle = serve(Arc::new(SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    }))
    .await
    .unwrap();
    let addr = handle.local_addr.to_string();

    // Two clients, so the second can observe what the world thinks is in it.
    let mut victim = login(&addr).await;
    let mut observer = login(&addr).await;

    if let Err(seen) = wait_for_avatar_count(&mut observer, 2).await {
        panic!("two connected clients should be two avatars, saw {seen}");
    }

    // The victim's connection dies on an undecodable body.
    victim.send(Bytes::from(undecodable_frame())).await.unwrap();

    if let Err(seen) = wait_for_avatar_count(&mut observer, 1).await {
        panic!("the dead connection leaked its avatar: still {seen} in the world");
    }
}
