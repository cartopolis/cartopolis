//! The picture store behind private chats: a signed-in account stores bytes
//! the server cannot read, another signed-in account fetches them by hash, and
//! nobody without a session gets anything.

use std::sync::Arc;

use bytes::Bytes;
use cartopolis_core::picture_limits::MAX_PICTURE_BYTES;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, NetMessage, PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, serve, SimConfig, SimHandle};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

async fn server() -> (SimHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pictures.db").to_str().unwrap().to_string();
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

/// Sign in over TCP and keep the connection open; returns its session token.
async fn token(
    handle: &SimHandle,
    username: &str,
) -> (Framed<TcpStream, LengthDelimitedCodec>, String) {
    let stream = TcpStream::connect(handle.local_addr).await.unwrap();
    let mut c = Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(32 << 20)
            .little_endian()
            .new_codec(),
    );
    let hello = NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth: AuthCredential::Password {
            username: username.into(),
            password: "pw".into(),
        },
    };
    c.send(Bytes::from(encode_app_frame(&hello).unwrap()))
        .await
        .unwrap();
    let ack = decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap();
    let NetMessage::ServerHelloAck { session_token, .. } = ack else {
        panic!("{ack:?}")
    };
    (c, session_token)
}

async fn post(url: String, token: Option<String>, body: Vec<u8>) -> (u16, serde_json::Value) {
    tokio::task::spawn_blocking(move || {
        let mut req = ureq::post(&url).set("Content-Type", "application/octet-stream");
        if let Some(t) = token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        let json = |r: ureq::Response| {
            serde_json::from_str(&r.into_string().unwrap_or_default()).unwrap_or_default()
        };
        match req.send_bytes(&body) {
            Ok(r) => (r.status(), json(r)),
            Err(ureq::Error::Status(code, r)) => (code, json(r)),
            Err(e) => panic!("{e}"),
        }
    })
    .await
    .unwrap()
}

async fn get(url: String, token: Option<String>) -> (u16, Vec<u8>) {
    tokio::task::spawn_blocking(move || {
        let mut req = ureq::get(&url);
        if let Some(t) = token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        match req.call() {
            Ok(r) => {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut r.into_reader(), &mut bytes).unwrap();
                (200, bytes)
            }
            Err(ureq::Error::Status(code, _)) => (code, Vec::new()),
            Err(e) => panic!("{e}"),
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_picture_is_stored_by_one_account_and_fetched_by_another() {
    let (s, _dir) = server().await;
    let http = format!("http://{}", s.http_addr.unwrap());
    let (_a, alice) = token(&s, "alice").await;
    let (_b, bob) = token(&s, "bob").await;

    // What a client uploads is ciphertext; the store takes any bytes.
    let sealed: Vec<u8> = (0..5000u32).map(|i| (i * 7919 % 251) as u8).collect();
    let (status, row) = post(
        format!("{http}/api/pictures"),
        Some(alice.clone()),
        sealed.clone(),
    )
    .await;
    assert_eq!(status, 201, "{row}");
    let hash = row["hash"].as_str().unwrap().to_string();
    assert_eq!(hash.len(), 64);

    // Somebody else in the chat fetches it by hash, byte for byte.
    let (status, bytes) = get(format!("{http}/api/pictures/{hash}"), Some(bob)).await;
    assert_eq!(status, 200);
    assert_eq!(bytes, sealed);

    // Nobody without a session, either way.
    let (status, _) = get(format!("{http}/api/pictures/{hash}"), None).await;
    assert_eq!(status, 401);
    let (status, _) = post(format!("{http}/api/pictures"), None, sealed).await;
    assert_eq!(status, 401);

    // A name that is not a hash reads nothing, and an empty or oversized body
    // is refused.
    let (status, _) = get(
        format!("{http}/api/pictures/..%2Fpictures.db"),
        Some(alice.clone()),
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = post(
        format!("{http}/api/pictures"),
        Some(alice.clone()),
        Vec::new(),
    )
    .await;
    assert_eq!(status, 400);
    let (status, _) = post(
        format!("{http}/api/pictures"),
        Some(alice),
        vec![0; MAX_PICTURE_BYTES + 1],
    )
    .await;
    assert_eq!(status, 413);
}
