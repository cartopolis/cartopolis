//! Video on a prim (protocol 28), end to end: a clip goes into the video store
//! over HTTP, a player starts it on their prim over the wire, everybody is told,
//! an ordinary edit does not stop it, and somebody else's prim is refused.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, GeoAnchor, NetMessage, PrimGeometry,
    PrimSurface, SetVideo, PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, serve, SimConfig, SimHandle};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Client = Framed<TcpStream, LengthDelimitedCodec>;

/// One second of 64×48 VP8 with Opus sound, made by ffmpeg — a clip this
/// module did not write, which is the point of a fixture for a parser.
const TINY: &[u8] = include_bytes!("fixtures/tiny.webm");

async fn server() -> (SimHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("video.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, "alice", "pw").unwrap();
        ensure_account(&conn, "bob", "pw").unwrap();
    }
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        http_listen: Some("127.0.0.1:0".into()),
        asset_dir: dir.path().join("assets").to_str().unwrap().to_string(),
        ..SimConfig::default()
    };
    (serve(Arc::new(config)).await.unwrap(), dir)
}

/// Sign in over TCP; returns the connection and its session token.
async fn connect(handle: &SimHandle, username: &str) -> (Client, String) {
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
    let NetMessage::ServerHelloAck { session_token, .. } = ack else {
        panic!("{ack:?}")
    };
    (c, session_token)
}

async fn send(c: &mut Client, msg: &NetMessage) {
    c.send(Bytes::from(encode_app_frame(msg).unwrap()))
        .await
        .unwrap();
}

async fn next<T>(c: &mut Client, mut f: impl FnMut(NetMessage) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
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

async fn post(url: String, token: &str, body: Vec<u8>) -> (u16, serde_json::Value) {
    let auth = format!("Bearer {token}");
    tokio::task::spawn_blocking(move || {
        match ureq::post(&url)
            .set("Authorization", &auth)
            .set("Content-Type", "video/webm")
            .send_bytes(&body)
        {
            Ok(r) => (r.status(), json(r)),
            Err(ureq::Error::Status(code, r)) => (code, json(r)),
            Err(e) => panic!("{e}"),
        }
    })
    .await
    .unwrap()
}

fn json(r: ureq::Response) -> serde_json::Value {
    serde_json::from_str(&r.into_string().unwrap_or_default()).unwrap_or_default()
}

async fn get(url: String, token: &str) -> (u16, Vec<u8>) {
    let auth = format!("Bearer {token}");
    tokio::task::spawn_blocking(
        move || match ureq::get(&url).set("Authorization", &auth).call() {
            Ok(r) => {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut r.into_reader(), &mut bytes).unwrap();
                (200, bytes)
            }
            Err(ureq::Error::Status(code, _)) => (code, Vec::new()),
            Err(e) => panic!("{e}"),
        },
    )
    .await
    .unwrap()
}

fn create(name: &str) -> NetMessage {
    NetMessage::CreatePrim {
        request_id: 1,
        name: name.into(),
        position: glam::Vec3::new(1.0, 1.0, 1.0),
        rotation: glam::Vec3::ZERO,
        scale: glam::Vec3::ONE,
        color: [0.5, 0.5, 0.5],
        texture_id: None,
        shape: "box".into(),
        surface: PrimSurface::default(),
        geometry: PrimGeometry::default(),
        geo: GeoAnchor {
            latitude: 0.0,
            longitude: 0.0,
        },
        particles: None,
        model: None,
    }
}

#[tokio::test]
async fn a_clip_is_uploaded_started_on_a_prim_and_survives_an_edit() {
    let (s, _dir) = server().await;
    let http = format!("http://{}", s.http_addr.unwrap());
    let (mut a, token) = connect(&s, "alice").await;
    let (mut b, bob_token) = connect(&s, "bob").await;

    // The store takes a real clip and measures it, and refuses what is not one.
    let (status, row) = post(format!("{http}/api/videos"), &token, TINY.to_vec()).await;
    assert_eq!(status, 201, "{row}");
    assert_eq!(
        (row["width"].as_u64(), row["height"].as_u64()),
        (Some(64), Some(48))
    );
    assert_eq!(row["has_audio"], serde_json::json!(true));
    let hash = row["hash"].as_str().unwrap().to_string();
    let (status, why) = post(format!("{http}/api/videos"), &token, b"a glb, say".to_vec()).await;
    assert_eq!(status, 400);
    assert!(why["error"].as_str().unwrap().contains("WebM"), "{why}");
    // And gives it back, to anyone signed in.
    let (status, bytes) = get(format!("{http}/api/videos/{hash}.webm"), &bob_token).await;
    assert_eq!((status, bytes.as_slice()), (200, TINY));

    // Alice builds a screen and starts the clip on it; Bob sees it start.
    send(&mut a, &create("screen")).await;
    let id = next(&mut a, |m| match m {
        NetMessage::PrimUpsert { prim } => Some(prim.id),
        _ => None,
    })
    .await;
    send(
        &mut a,
        &NetMessage::SetPrimVideo {
            request_id: 2,
            prim_id: id,
            video: Some(SetVideo {
                asset: hash.clone(),
                looped: true,
            }),
        },
    )
    .await;
    let seen = next(&mut b, |m| match m {
        NetMessage::PrimUpsert { prim } if prim.id == id && prim.video.is_some() => prim.video,
        _ => None,
    })
    .await;
    assert_eq!(seen.asset, hash);
    assert!(seen.looped);
    assert!(seen.started_at_ms > 0);

    // A colour change — a full-state edit that knows nothing of video — does
    // not stop it.
    send(
        &mut a,
        &NetMessage::UpdatePrim {
            request_id: 3,
            prim_id: id,
            position: glam::Vec3::new(1.0, 1.0, 1.0),
            rotation: glam::Vec3::ZERO,
            scale: glam::Vec3::ONE,
            color: [1.0, 0.0, 0.0],
            texture_id: None,
            name: "screen".into(),
            surface: PrimSurface::default(),
            geometry: PrimGeometry::default(),
            geo: GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
            particles: None,
            model: None,
        },
    )
    .await;
    let after = next(&mut b, |m| match m {
        NetMessage::PrimUpsert { prim } if prim.id == id && prim.color == [1.0, 0.0, 0.0] => {
            Some(prim.video)
        }
        _ => None,
    })
    .await;
    assert_eq!(after, Some(seen));

    // Bob may not start or stop a clip on Alice's screen.
    send(
        &mut b,
        &NetMessage::SetPrimVideo {
            request_id: 4,
            prim_id: id,
            video: None,
        },
    )
    .await;
    let code = next(&mut b, |m| match m {
        NetMessage::ServerError {
            request_id: 4,
            code,
            ..
        } => Some(code),
        _ => None,
    })
    .await;
    assert_eq!(code, 403);

    // Nor may anyone play a clip the store does not have.
    send(
        &mut a,
        &NetMessage::SetPrimVideo {
            request_id: 5,
            prim_id: id,
            video: Some(SetVideo {
                asset: "0".repeat(64),
                looped: false,
            }),
        },
    )
    .await;
    let why = next(&mut a, |m| match m {
        NetMessage::ServerError {
            request_id: 5,
            message,
            ..
        } => Some(message),
        _ => None,
    })
    .await;
    assert!(why.contains("does not have that video"), "{why}");
}
