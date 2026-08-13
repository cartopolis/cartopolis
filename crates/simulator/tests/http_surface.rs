//! The simulator's HTTP surface, end to end: the browser's WebSocket carrier and
//! the account admin API, against a real listener on an ephemeral port.
//!
//! Two things are being proved, and they are the two that a unit test cannot
//! reach. First, that a client arriving over WebSocket is the *same* client to
//! everything above the transport — same `ClientHello`, same `PROTOCOL_VERSION`,
//! same session token — because the whole point of the `FrameTransport` refactor
//! was that there is one connection loop rather than two that drift. Second, that
//! an account created through the admin API can actually sign in and play, which
//! is the only definition of "manage accounts" that means anything: a GUI that
//! writes rows nobody can log in with is a GUI that works and a server that
//! doesn't.

use std::sync::Arc;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, NetMessage, PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, ensure_admin_account, serve, SimConfig, SimHandle};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

/// A server with both listeners up, an admin and a plain player provisioned.
async fn server() -> (SimHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("http.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_admin_account(&conn, "root", "adminpass").unwrap();
        ensure_account(&conn, "player", "playerpw").unwrap();
    }
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        http_listen: Some("127.0.0.1:0".into()),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    (handle, dir)
}

fn http_base(handle: &SimHandle) -> String {
    format!("http://{}", handle.http_addr.expect("http listener bound"))
}

/// `ureq` is blocking, so every admin call goes through `spawn_blocking` rather
/// than parking a runtime worker on a socket read.
async fn api(
    method: &'static str,
    url: String,
    token: Option<String>,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    tokio::task::spawn_blocking(move || {
        let mut req = ureq::request(method, &url);
        if let Some(token) = token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        // `send_string` with an explicit content type rather than `send_json`:
        // that method is behind ureq's `json` feature, and turning it on here
        // would turn it on for the *client's* copy of ureq too under workspace
        // feature unification, for a test.
        let result = match body {
            Some(json) => req
                .set("Content-Type", "application/json")
                .send_string(&json.to_string()),
            None => req.call(),
        };
        let response = match result {
            Ok(r) => r,
            // A 4xx is an `Err` in ureq and is exactly what several of these
            // assertions are about, so it is a result, not a failure.
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => panic!("transport error: {e}"),
        };
        let status = response.status();
        let text = response.into_string().unwrap_or_default();
        let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        (status, json)
    })
    .await
    .unwrap()
}

async fn admin_token(handle: &SimHandle, username: &str, password: &str) -> (u16, Option<String>) {
    let (status, body) = api(
        "POST",
        format!("{}/api/admin/login", http_base(handle)),
        None,
        Some(serde_json::json!({"username": username, "password": password})),
    )
    .await;
    let token = body
        .get("token")
        .and_then(|t| t.as_str())
        .map(str::to_string);
    (status, token)
}

/// A `ClientHello` over the WebSocket carrier: one binary message carrying the
/// same frame body the TCP codec would have length-prefixed.
async fn ws_hello(handle: &SimHandle, auth: AuthCredential) -> NetMessage {
    let url = format!("ws://{}/ws", handle.http_addr.unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let frame = encode_app_frame(&NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth,
    })
    .unwrap();
    socket.send(Message::Binary(frame.into())).await.unwrap();
    loop {
        match socket.next().await.expect("server closed").unwrap() {
            Message::Binary(bytes) => return decode_app_frame(&bytes).unwrap(),
            // A keepalive is not an answer; keep reading.
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected a binary frame, got {other:?}"),
        }
    }
}

async fn tcp_hello(handle: &SimHandle, auth: AuthCredential) -> NetMessage {
    let stream = TcpStream::connect(handle.local_addr).await.unwrap();
    let mut client = Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(32 << 20)
            .little_endian()
            .new_codec(),
    );
    let frame = encode_app_frame(&NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth,
    })
    .unwrap();
    client.send(Bytes::from(frame)).await.unwrap();
    decode_app_frame(&client.next().await.unwrap().unwrap()).unwrap()
}

/// The browser carrier authenticates exactly as the TCP one does — same message,
/// same version check, same token.
#[tokio::test]
async fn a_websocket_client_completes_the_same_handshake() {
    let (handle, _dir) = server().await;

    let token = match ws_hello(
        &handle,
        AuthCredential::Password {
            username: "player".into(),
            password: "playerpw".into(),
        },
    )
    .await
    {
        NetMessage::ServerHelloAck {
            session_token,
            username,
            ..
        } => {
            assert_eq!(username, "player");
            assert!(!session_token.is_empty());
            session_token
        }
        other => panic!("expected ServerHelloAck over websocket, got {other:?}"),
    };

    // The session store is shared across carriers, so a token minted in the
    // browser reconnects over TCP. That is the property that makes this one
    // server rather than two that happen to share a database.
    assert!(
        matches!(
            tcp_hello(&handle, AuthCredential::Token(token)).await,
            NetMessage::ServerHelloAck { .. }
        ),
        "a token issued over websocket should authenticate over tcp"
    );

    assert!(
        matches!(
            ws_hello(
                &handle,
                AuthCredential::Password {
                    username: "player".into(),
                    password: "wrong".into(),
                },
            )
            .await,
            NetMessage::ServerError { code: 401, .. }
        ),
        "a bad password must be refused on the websocket carrier too"
    );
}

/// The whole admin loop: sign in, create a player, and have that player sign in
/// to the *game*. If the last step fails, the GUI is writing rows nobody can use.
#[tokio::test]
async fn an_account_created_through_the_api_can_sign_in() {
    let (handle, _dir) = server().await;
    let (status, token) = admin_token(&handle, "root", "adminpass").await;
    assert_eq!(status, 200, "the admin should be able to sign in");
    let token = token.unwrap();

    let (status, body) = api(
        "POST",
        format!("{}/api/admin/accounts", http_base(&handle)),
        Some(token.clone()),
        Some(serde_json::json!({"username": "newcomer", "password": "letmein123"})),
    )
    .await;
    assert_eq!(status, 201, "create should succeed: {body}");
    assert_eq!(body["account"]["username"], "newcomer");
    assert_eq!(
        body["account"]["admin"], false,
        "new accounts are not admins"
    );

    assert!(
        matches!(
            tcp_hello(
                &handle,
                AuthCredential::Password {
                    username: "newcomer".into(),
                    password: "letmein123".into(),
                },
            )
            .await,
            NetMessage::ServerHelloAck { .. }
        ),
        "an account created through the admin API must be able to log in"
    );

    // A second create under the same name is a conflict, not a silent overwrite
    // of someone's password.
    let (status, _) = api(
        "POST",
        format!("{}/api/admin/accounts", http_base(&handle)),
        Some(token.clone()),
        Some(serde_json::json!({"username": "newcomer", "password": "different"})),
    )
    .await;
    assert_eq!(status, 409);

    let (status, body) = api(
        "GET",
        format!("{}/api/admin/accounts", http_base(&handle)),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let names: Vec<&str> = body["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["username"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"root") && names.contains(&"newcomer"));
    assert!(
        !body.to_string().contains("$argon2"),
        "the listing must not carry password hashes"
    );
}

/// Disabling an account closes the door on the next handshake. (It does not
/// evict a session already established — that is `Sessions::revoke`'s job and is
/// documented on `auth::set_disabled`.)
#[tokio::test]
async fn disabling_an_account_stops_the_next_login() {
    let (handle, _dir) = server().await;
    let (_, token) = admin_token(&handle, "root", "adminpass").await;
    let token = token.unwrap();

    let (_, body) = api(
        "GET",
        format!("{}/api/admin/accounts", http_base(&handle)),
        Some(token.clone()),
        None,
    )
    .await;
    let player_id = body["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["username"] == "player")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = api(
        "PATCH",
        format!("{}/api/admin/accounts/{player_id}", http_base(&handle)),
        Some(token),
        Some(serde_json::json!({"disabled": true})),
    )
    .await;
    assert_eq!(status, 200);

    assert!(
        matches!(
            tcp_hello(
                &handle,
                AuthCredential::Password {
                    username: "player".into(),
                    password: "playerpw".into(),
                },
            )
            .await,
            NetMessage::ServerError { code: 401, .. }
        ),
        "a disabled account must not be able to log in"
    );
}

/// The API is admin-only, and a player's *correct* password gets the same answer
/// as a wrong one — so the endpoint cannot be used to enumerate usernames.
#[tokio::test]
async fn a_player_cannot_reach_the_admin_api() {
    let (handle, _dir) = server().await;

    let (status, token) = admin_token(&handle, "player", "playerpw").await;
    assert_eq!(status, 401, "a non-admin must not get a token");
    assert!(token.is_none());

    let (wrong_status, _) = admin_token(&handle, "player", "definitely-wrong").await;
    assert_eq!(
        wrong_status, status,
        "a correct non-admin password must be indistinguishable from a wrong one"
    );

    // And no token at all is a 401 rather than an open door.
    let (status, _) = api(
        "GET",
        format!("{}/api/admin/accounts", http_base(&handle)),
        None,
        None,
    )
    .await;
    assert_eq!(status, 401);

    let (status, _) = api(
        "GET",
        format!("{}/api/admin/accounts", http_base(&handle)),
        Some("not-a-real-token".into()),
        None,
    )
    .await;
    assert_eq!(status, 401);
}

/// The last enabled admin cannot demote, disable or delete themselves. Locking
/// everyone out of a running server is only repairable with shell access to the
/// box it runs on.
#[tokio::test]
async fn the_last_admin_cannot_be_removed() {
    let (handle, _dir) = server().await;
    let (_, token) = admin_token(&handle, "root", "adminpass").await;
    let token = token.unwrap();

    let (_, body) = api(
        "GET",
        format!("{}/api/admin/me", http_base(&handle)),
        Some(token.clone()),
        None,
    )
    .await;
    let root_id = body["principal"].as_str().unwrap().to_string();

    for patch in [
        serde_json::json!({"admin": false}),
        serde_json::json!({"disabled": true}),
    ] {
        let (status, body) = api(
            "PATCH",
            format!("{}/api/admin/accounts/{root_id}", http_base(&handle)),
            Some(token.clone()),
            Some(patch.clone()),
        )
        .await;
        assert_eq!(status, 409, "{patch} should have been refused: {body}");
    }

    let (status, _) = api(
        "DELETE",
        format!("{}/api/admin/accounts/{root_id}", http_base(&handle)),
        Some(token.clone()),
        None,
    )
    .await;
    assert_eq!(status, 409);

    // With a second admin in place, the first may stand down.
    let (status, body) = api(
        "POST",
        format!("{}/api/admin/accounts", http_base(&handle)),
        Some(token.clone()),
        Some(serde_json::json!({"username": "second", "password": "secondpass", "admin": true})),
    )
    .await;
    assert_eq!(status, 201, "{body}");

    let (status, _) = api(
        "PATCH",
        format!("{}/api/admin/accounts/{root_id}", http_base(&handle)),
        Some(token),
        Some(serde_json::json!({"admin": false})),
    )
    .await;
    assert_eq!(status, 200, "with a spare admin, standing down is allowed");
}
