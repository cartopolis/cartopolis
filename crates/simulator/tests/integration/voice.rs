//! End-to-end proximity voice over loopback TCP.
//!
//! Voice reuses the chat scopes and `ChatHub`'s fan-out with an opaque payload
//! in place of the string, so what these cover is the ways voice must *differ*
//! from text: no echo back to the speaker, nothing written to scrollback, and no
//! global tier.
//!
//! The radius cutoff itself is unit-tested in `chat::tests` — both clients here
//! authenticate as the same account and spawn co-located, and `SimHandle`
//! exposes no way to move them apart.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, ChatScope, NetMessage, PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, serve, SimConfig, SimHandle};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Client = Framed<TcpStream, LengthDelimitedCodec>;

async fn start_sim() -> (SimHandle, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("voice.db").to_str().unwrap().to_string();
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
    (handle, addr, dir)
}

/// The first intent, standing still: what gives a connection's avatar a body
/// (`AvatarSim::embodied`). Until a client sends one it is in no snapshot and
/// in earshot of nobody, which is how a chat-only web UI stays out of the world.
fn stand_here() -> Bytes {
    Bytes::from(
        encode_app_frame(&NetMessage::ClientIntent {
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
        })
        .unwrap(),
    )
}

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
    let ack = decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap();
    let NetMessage::ServerHelloAck { your_avatar_id, .. } = ack else {
        panic!("expected ServerHelloAck, got {ack:?}");
    };
    c.send(stand_here()).await.unwrap();
    // Every other connection is concurrent with this one, so wait until the
    // world has applied that intent — until a snapshot shows this avatar — or a
    // test that speaks next can race it and find nobody in earshot.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = c.next().await.unwrap().unwrap();
            if let Ok(NetMessage::WorldSnapshot { avatars, .. }) = decode_app_frame(&frame) {
                if avatars.iter().any(|a| a.id == your_avatar_id) {
                    break;
                }
            }
        }
    })
    .await
    .expect("this avatar never appeared in the world");
    c
}

fn say(seq: u32, payload: &[u8]) -> Bytes {
    Bytes::from(
        encode_app_frame(&NetMessage::VoiceFrame {
            scope: ChatScope::Local { shout: false },
            seq,
            payload: payload.to_vec(),
        })
        .unwrap(),
    )
}

/// Next `VoiceData`, skipping snapshots. `None` if none arrives in `ms`.
async fn recv_voice(c: &mut Client, ms: u64) -> Option<(u64, u32, Vec<u8>, Option<f32>)> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        let frame = tokio::time::timeout_at(deadline, c.next())
            .await
            .ok()??
            .ok()?;
        if let NetMessage::VoiceData {
            sender_id,
            seq,
            payload,
            distance_m,
            ..
        } = decode_app_frame(&frame).unwrap()
        {
            return Some((sender_id, seq, payload, distance_m));
        }
    }
}

#[tokio::test]
async fn proximity_voice_reaches_a_listener_with_a_distance() {
    let (_handle, addr, _dir) = start_sim().await;
    let mut a = connect_auth(&addr).await;
    let mut b = connect_auth(&addr).await;

    a.send(say(7, b"opus-ish")).await.unwrap();

    let (sender_id, seq, payload, distance_m) =
        recv_voice(&mut b, 3000).await.expect("no voice arrived");
    assert_eq!(seq, 7, "the sender's frame counter survives the hop");
    assert_eq!(
        payload, b"opus-ish",
        "the sim forwards the payload verbatim"
    );
    assert!(sender_id != 0);
    // Proximity always carries a distance; it is what the client attenuates by.
    assert!(distance_m.is_some(), "Local voice must carry distance_m");
}

#[tokio::test]
async fn a_speaker_does_not_hear_themselves() {
    let (_handle, addr, _dir) = start_sim().await;
    let mut a = connect_auth(&addr).await;
    let mut b = connect_auth(&addr).await;

    a.send(say(1, b"echo?")).await.unwrap();

    // B hears it, so it was delivered at all...
    assert!(recv_voice(&mut b, 3000).await.is_some());
    // ...and A does not, which for audio is an echo rather than a confirmation.
    assert!(
        recv_voice(&mut a, 500).await.is_none(),
        "voice must not loop back to the speaker"
    );
}

#[tokio::test]
async fn global_voice_is_dropped() {
    let (_handle, addr, _dir) = start_sim().await;
    let mut a = connect_auth(&addr).await;
    let mut b = connect_auth(&addr).await;

    let frame = encode_app_frame(&NetMessage::VoiceFrame {
        scope: ChatScope::Global,
        seq: 1,
        payload: b"everyone".to_vec(),
    })
    .unwrap();
    a.send(Bytes::from(frame)).await.unwrap();

    assert!(
        recv_voice(&mut b, 500).await.is_none(),
        "global voice is deliberately not carried"
    );
}

#[tokio::test]
async fn an_oversize_payload_is_refused_without_killing_the_link() {
    let (_handle, addr, _dir) = start_sim().await;
    let mut a = connect_auth(&addr).await;
    let mut b = connect_auth(&addr).await;

    // Past MAX_VOICE_BYTES (1500): dropped, and the connection survives it.
    a.send(say(1, &vec![0u8; 4096])).await.unwrap();
    assert!(recv_voice(&mut b, 500).await.is_none());

    a.send(say(2, b"still here")).await.unwrap();
    let (_, seq, _, _) = recv_voice(&mut b, 3000)
        .await
        .expect("link died on an oversize frame");
    assert_eq!(seq, 2);
}

#[tokio::test]
async fn voice_is_never_stored_as_scrollback() {
    let (_handle, addr, _dir) = start_sim().await;
    let mut a = connect_auth(&addr).await;
    let mut b = connect_auth(&addr).await;

    a.send(say(1, b"ephemeral")).await.unwrap();
    assert!(recv_voice(&mut b, 3000).await.is_some());

    // A client connecting afterward replays scrollback. Voice must not be in it:
    // it is ephemeral in every scope, and audio in a database is a liability.
    let mut c = connect_auth(&addr).await;
    assert!(
        recv_voice(&mut c, 500).await.is_none(),
        "voice must not be replayed from scrollback"
    );
}

#[tokio::test]
async fn voice_needs_authentication() {
    let (_handle, addr, _dir) = start_sim().await;
    let stream = TcpStream::connect(&addr).await.unwrap();
    let mut raw = Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(32 << 20)
            .little_endian()
            .new_codec(),
    );

    // No ClientHello: speak straight into the socket.
    raw.send(say(1, b"unauthenticated")).await.unwrap();

    // The sim must not answer with voice — it has no session to attribute it to.
    let got = tokio::time::timeout(Duration::from_millis(500), raw.next()).await;
    if let Ok(Some(Ok(frame))) = got {
        assert!(
            !matches!(decode_app_frame(&frame), Ok(NetMessage::VoiceData { .. })),
            "voice reached an unauthenticated connection"
        );
    }
}
