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
use cartopolis_simulator::{
    ensure_account, ensure_admin_account, registration, serve, SimConfig, SimHandle,
};
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

/// `/api/login` is the player's way back in: an already-registered account signs
/// in with its password and holds an ordinary session — enough for the
/// `require_session` routes (claiming a house), and still nothing on the admin
/// surface.
#[tokio::test]
async fn a_player_can_sign_in_and_hold_a_session() {
    let (handle, _dir) = server().await;
    let base = http_base(&handle);

    let (status, body) = api(
        "POST",
        format!("{base}/api/login"),
        None,
        Some(serde_json::json!({"username": "player", "password": "playerpw"})),
    )
    .await;
    assert_eq!(status, 200);
    let token = body["token"].as_str().expect("a session token").to_string();

    // The session works where any session works…
    let (status, _) = api(
        "GET",
        format!("{base}/api/buildings/mine"),
        Some(token.clone()),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "a player session must reach the building routes"
    );

    // …and not where only an admin's does.
    let (status, _) = api(
        "GET",
        format!("{base}/api/admin/accounts"),
        Some(token.clone()),
        None,
    )
    .await;
    assert_eq!(
        status, 403,
        "a player session must not open the admin surface"
    );

    // A wrong password is refused, and sign-out revokes the session.
    let (status, _) = api(
        "POST",
        format!("{base}/api/login"),
        None,
        Some(serde_json::json!({"username": "player", "password": "wrong"})),
    )
    .await;
    assert_eq!(status, 401);

    let (status, _) = api(
        "POST",
        format!("{base}/api/logout"),
        Some(token.clone()),
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (status, _) = api(
        "GET",
        format!("{base}/api/buildings/mine"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, 401, "a revoked session must be dead");
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

// ---------------------------------------------------------------------------
// Self-service registration
// ---------------------------------------------------------------------------

/// The verification token, as the person who received the mail has it.
///
/// The test cannot read the mail — the server is in its logging mode — and the
/// stored copy is a hash, which is the point of storing it that way. So it mints
/// a fresh one through the *same* call the re-send endpoint makes, against the
/// same database file. That is a real link for a real pending account, and it is
/// the only way to hold one without either weakening the storage or adding a test
/// seam to the API that a deployment would also have.
fn link_token(db: &str, email: &str) -> String {
    let conn = cartopolis_simulator::db::open_and_migrate(db).unwrap();
    let pending = registration::pending_by_email(&conn, email)
        .unwrap()
        .expect("a pending registration for that address");
    registration::reissue(&conn, &pending).unwrap()
}

/// The whole point of the feature: the account exists from the moment the form is
/// submitted (so the name is held) and cannot be signed in with until the address
/// answers.
#[tokio::test]
async fn a_registration_cannot_sign_in_until_the_link_is_opened() {
    let (handle, dir) = server().await;
    let db = dir.path().join("http.db").to_str().unwrap().to_string();
    let base = http_base(&handle);

    let (status, body) = api(
        "POST",
        format!("{base}/api/register"),
        None,
        Some(serde_json::json!({
            "username": "ada",
            "email": "Ada@Example.com",
            "password": "correcthorse",
        })),
    )
    .await;
    assert_eq!(status, 202, "registration accepted: {body}");

    let credential = || AuthCredential::Password {
        username: "ada".into(),
        password: "correcthorse".into(),
    };

    // Refused, and refused with a sentence that says what to do — "authentication
    // failed" here is the bug this test exists to keep fixed.
    match tcp_hello(&handle, credential()).await {
        NetMessage::ServerError { code, message, .. } => {
            assert_eq!(code, 401);
            assert!(
                message.contains("email"),
                "a pending account should be told why, got {message:?}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    let token = link_token(&db, "ada@example.com");
    let (status, body) = api(
        "POST",
        format!("{base}/api/register/verify"),
        None,
        Some(serde_json::json!({ "token": token })),
    )
    .await;
    assert_eq!(status, 200, "verification: {body}");
    assert_eq!(body["username"], "ada");

    // ...and now the same credential plays, over either carrier.
    assert!(matches!(
        tcp_hello(&handle, credential()).await,
        NetMessage::ServerHelloAck { .. }
    ));
    assert!(matches!(
        ws_hello(&handle, credential()).await,
        NetMessage::ServerHelloAck { .. }
    ));

    // A spent link is refused rather than silently re-confirming.
    let (status, _) = api(
        "POST",
        format!("{base}/api/register/verify"),
        None,
        Some(serde_json::json!({ "token": token })),
    )
    .await;
    assert_eq!(status, 404);
}

/// The way through when the verification mail never arrives: an admin activates
/// the account, and the same password then plays. Only an admin may, and only
/// once.
#[tokio::test]
async fn an_admin_can_activate_a_registration_by_hand() {
    let (handle, _dir) = server().await;
    let base = http_base(&handle);
    let (status, _) = api(
        "POST",
        format!("{base}/api/register"),
        None,
        Some(serde_json::json!({
            "username": "grace",
            "email": "grace@example.com",
            "password": "correcthorse",
        })),
    )
    .await;
    assert_eq!(status, 202);

    let (_, admin) = admin_token(&handle, "root", "adminpass").await;
    let (_, list) = api(
        "GET",
        format!("{base}/api/admin/accounts"),
        admin.clone(),
        None,
    )
    .await;
    let id = list["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["username"] == "grace")
        .expect("the pending account is listed")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let url = format!("{base}/api/admin/accounts/{id}/activate");

    let player = api(
        "POST",
        format!("{base}/api/login"),
        None,
        Some(serde_json::json!({"username": "player", "password": "playerpw"})),
    )
    .await
    .1["token"]
        .as_str()
        .map(str::to_string);
    let (status, _) = api("POST", url.clone(), player, None).await;
    assert_eq!(status, 403, "a player may not activate accounts");

    let (status, body) = api("POST", url.clone(), admin.clone(), None).await;
    assert_eq!(status, 200, "activation: {body}");
    assert_eq!(body["account"]["email_pending"], false);
    // The test server has no relay, so nothing actually left the process.
    assert_eq!(body["mailed"], false);

    assert!(matches!(
        tcp_hello(
            &handle,
            AuthCredential::Password {
                username: "grace".into(),
                password: "correcthorse".into(),
            }
        )
        .await,
        NetMessage::ServerHelloAck { .. }
    ));

    let (status, _) = api("POST", url, admin, None).await;
    assert_eq!(status, 409, "a second activation is refused");
}

/// A taken *name* has to be reported — the form cannot be completed otherwise. A
/// taken *address* must not be, or the form answers "does this person have an
/// account here?" for anyone who types an address into it.
#[tokio::test]
async fn a_taken_name_is_reported_and_a_taken_address_is_not() {
    let (handle, _dir) = server().await;
    let base = http_base(&handle);

    let register = |username: &'static str, email: &'static str| {
        let base = base.clone();
        async move {
            api(
                "POST",
                format!("{base}/api/register"),
                None,
                Some(serde_json::json!({
                    "username": username,
                    "email": email,
                    "password": "correcthorse",
                })),
            )
            .await
        }
    };

    assert_eq!(register("ada", "ada@example.com").await.0, 202);

    let (status, body) = register("ada", "someone@example.com").await;
    assert_eq!(status, 409);
    assert!(body["error"].as_str().unwrap_or_default().contains("taken"));

    // Same address, different name: accepted, with the same sentence a real
    // registration gets. No account is created — the person who owns the address
    // is told by mail instead.
    let (status, body) = register("grace", "ADA@example.com").await;
    assert_eq!(status, 202, "{body}");
    let (status, _) = api(
        "POST",
        format!("{base}/api/admin/accounts"),
        Some(admin_token(&handle, "root", "adminpass").await.1.unwrap()),
        Some(serde_json::json!({"username": "grace", "password": "correcthorse"})),
    )
    .await;
    assert_eq!(
        status, 201,
        "the second registration must not have taken the name grace"
    );
}

/// An operator can run a private world. The endpoint refuses rather than
/// disappearing, so the sign-up form is told *why* it may not be used.
#[tokio::test]
async fn registration_can_be_closed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("closed.db").to_str().unwrap().to_string();
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        http_listen: Some("127.0.0.1:0".into()),
        registration_open: false,
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    let base = http_base(&handle);

    let (status, body) = api("GET", format!("{base}/api/register"), None, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["open"], false);

    let (status, _) = api(
        "POST",
        format!("{base}/api/register"),
        None,
        Some(serde_json::json!({
            "username": "ada",
            "email": "ada@example.com",
            "password": "correcthorse",
        })),
    )
    .await;
    assert_eq!(status, 403);
}

// ---------------------------------------------------------------------------
// Owning buildings
// ---------------------------------------------------------------------------

/// Everything about a claim that can be proved without reaching PDOK.
///
/// The register lookups themselves are covered by `buildings`' own `#[ignore]`d test
/// — they are somebody else's service and a suite that fails when it is down is a
/// suite people learn to disbelieve. What is worth pinning here is the shape
/// around them: who may ask, what an unresolvable address does, and that
/// confirming an email hands back a session the claim step can use.
#[tokio::test]
async fn a_building_is_claimed_with_the_session_confirmation_hands_back() {
    let (handle, dir) = server().await;
    let db = dir.path().join("http.db").to_str().unwrap().to_string();
    let base = http_base(&handle);

    // No session: no buildings.
    let (status, _) = api("GET", format!("{base}/api/buildings/mine"), None, None).await;
    assert_eq!(status, 401);

    api(
        "POST",
        format!("{base}/api/register"),
        None,
        Some(serde_json::json!({
            "username": "ada",
            "email": "ada@example.com",
            "password": "correcthorse",
        })),
    )
    .await;
    let token = link_token(&db, "ada@example.com");
    let (status, body) = api(
        "POST",
        format!("{base}/api/register/verify"),
        None,
        Some(serde_json::json!({ "token": token })),
    )
    .await;
    assert_eq!(status, 200);
    // Confirming the address is an authentication, so the claim screen that
    // follows does not have to be a second sign-in form.
    let session = body["token"]
        .as_str()
        .expect("verification hands back a session token")
        .to_string();

    let (status, body) = api(
        "GET",
        format!("{base}/api/buildings/mine"),
        Some(session.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body["buildings"].as_array().map(Vec::len),
        Some(0),
        "a new account owns nothing"
    );
    assert!(body["limit"].as_u64().unwrap_or(0) >= 1);

    // Atlas's question — may this session change that building? — answers no for
    // a building nobody owns, and says why.
    let (status, body) = api(
        "GET",
        format!("{base}/api/buildings/0014100010938796/access"),
        Some(session.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["username"], "ada");
    assert_eq!(body["owner"], false);
    assert_eq!(body["can_edit"], false);
    assert!(body["owned_by"].is_null());
    // Giving up a building you do not own is refused.
    let (status, _) = api(
        "DELETE",
        format!("{base}/api/buildings/0014100010938796"),
        Some(session.clone()),
        None,
    )
    .await;
    assert_eq!(status, 404);

    // An id the register cannot possibly hold is refused before any network call
    // — and refused as "not one of Groningen's", which is the rule being applied.
    let (status, body) = api(
        "POST",
        format!("{base}/api/buildings/claim"),
        Some(session.clone()),
        Some(serde_json::json!({ "nummeraanduiding_id": "not-an-id" })),
    )
    .await;
    assert_eq!(status, 404);
    assert!(body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("Groningen"));

    // The operator's dispute list exists and is empty.
    let admin = admin_token(&handle, "root", "adminpass").await.1.unwrap();
    let (status, body) = api(
        "GET",
        format!("{base}/api/admin/claims"),
        Some(admin.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["buildings"].as_array().map(Vec::len), Some(0));

    // An admin may change any building, owned or not.
    let (_, body) = api(
        "GET",
        format!("{base}/api/buildings/0014100010938796/access"),
        Some(admin.clone()),
        None,
    )
    .await;
    assert_eq!(body["admin"], true);
    assert_eq!(body["can_edit"], true);

    // ...and a player cannot read it.
    let (status, _) = api(
        "GET",
        format!("{base}/api/admin/claims"),
        Some(session),
        None,
    )
    .await;
    assert_eq!(status, 403);
}

/// Sign in over HTTP as a player would, for the routes a person manages.
async fn player_session(base: &str, username: &str, password: &str) -> String {
    let (status, body) = api(
        "POST",
        format!("{base}/api/login"),
        None,
        Some(serde_json::json!({"username": username, "password": password})),
    )
    .await;
    assert_eq!(status, 200, "{username} signs in");
    body["token"].as_str().unwrap().to_string()
}

async fn make_token(
    base: &str,
    session: &str,
    password: &str,
    elevated: bool,
) -> (u16, serde_json::Value) {
    api(
        "POST",
        format!("{base}/api/tokens"),
        Some(session.to_string()),
        Some(serde_json::json!({"label": "bot", "password": password, "elevated": elevated})),
    )
    .await
}

/// An access token is made from a sign-in with the password, signs in on both
/// carriers, acts as a player on an admin account unless made elevated, and can
/// manage nothing — not even itself.
#[tokio::test]
async fn an_access_token_is_made_used_and_limited() {
    let (handle, _dir) = server().await;
    let base = http_base(&handle);
    let session = player_session(&base, "root", "adminpass").await;

    let (status, _) = make_token(&base, &session, "wrong", false).await;
    assert_eq!(status, 403, "the password is asked again");
    let (status, body) = make_token(&base, &session, "adminpass", false).await;
    assert_eq!(status, 200);
    let plain = body["token"].as_str().unwrap().to_string();
    let (_, body) = make_token(&base, &session, "adminpass", true).await;
    let elevated = body["token"].as_str().unwrap().to_string();

    let get = |token: &str, path: &str| {
        api(
            "GET",
            format!("{base}{path}"),
            Some(token.to_string()),
            None,
        )
    };
    assert_eq!(
        get(&plain, "/api/buildings/mine").await.0,
        200,
        "a token works where a session does"
    );
    assert_eq!(
        get(&plain, "/api/admin/accounts").await.0,
        403,
        "a plain token on an admin is a player"
    );
    assert_eq!(
        get(&elevated, "/api/admin/accounts").await.0,
        200,
        "an elevated one is not"
    );
    assert_eq!(
        get(&plain, "/api/tokens").await.0,
        403,
        "a token cannot list tokens"
    );
    let (status, _) = make_token(&base, &plain, "adminpass", false).await;
    assert_eq!(
        status, 403,
        "a token cannot mint a sibling, even with the password"
    );

    let (status, list) = get(&session, "/api/tokens").await;
    assert_eq!(status, 200);
    assert_eq!(list["tokens"].as_array().unwrap().len(), 2);
    assert!(
        list.to_string().find(&plain).is_none(),
        "the list never carries a secret"
    );

    // On the wire the token signs in, and the ack hands back the token itself
    // rather than minting a session per connect.
    match tcp_hello(&handle, AuthCredential::Token(plain.clone())).await {
        NetMessage::ServerHelloAck { session_token, .. } => assert_eq!(session_token, plain),
        other => panic!("expected an ack, got {other:?}"),
    }
}

/// Revoking a token refuses it everywhere at once, and closes a connection it
/// already holds open — revoking is how a running bot is stopped.
#[tokio::test]
async fn revoking_a_token_ends_its_open_connection() {
    let (handle, _dir) = server().await;
    let base = http_base(&handle);
    let session = player_session(&base, "player", "playerpw").await;
    let (_, body) = make_token(&base, &session, "playerpw", false).await;
    let token = body["token"].as_str().unwrap().to_string();
    let id = body["info"]["id"].as_str().unwrap().to_string();

    // A live connection made with the token.
    let stream = TcpStream::connect(handle.local_addr).await.unwrap();
    let mut client = Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(32 << 20)
            .little_endian()
            .new_codec(),
    );
    let hello = encode_app_frame(&NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth: AuthCredential::Token(token.clone()),
    })
    .unwrap();
    client.send(Bytes::from(hello)).await.unwrap();
    assert!(matches!(
        decode_app_frame(&client.next().await.unwrap().unwrap()).unwrap(),
        NetMessage::ServerHelloAck { .. }
    ));

    // Somebody else's revoke finds nothing; the owner's works.
    let other = player_session(&base, "root", "adminpass").await;
    let (status, _) = api(
        "DELETE",
        format!("{base}/api/tokens/{id}"),
        Some(other),
        None,
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = api(
        "DELETE",
        format!("{base}/api/tokens/{id}"),
        Some(session),
        None,
    )
    .await;
    assert_eq!(status, 204);

    let (status, _) = api(
        "GET",
        format!("{base}/api/buildings/mine"),
        Some(token.clone()),
        None,
    )
    .await;
    assert_eq!(status, 401, "a revoked token is dead on HTTP");
    assert!(matches!(
        tcp_hello(&handle, AuthCredential::Token(token)).await,
        NetMessage::ServerError { code: 401, .. }
    ));

    // The open connection is told and closed within the check interval.
    let ended = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while let Some(frame) = client.next().await {
            let Ok(bytes) = frame else { return true };
            if let Ok(NetMessage::ServerError { code: 401, .. }) = decode_app_frame(&bytes) {
                return true;
            }
        }
        true
    })
    .await;
    assert_eq!(ended, Ok(true), "the connection outlived its revoked token");
}
