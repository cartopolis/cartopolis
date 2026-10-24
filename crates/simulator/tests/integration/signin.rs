//! Signing in with another account, end to end, against stand-ins for Discord
//! and for an OpenID server on this machine (`SimConfig::signin_test_base`).
//!
//! The provider's half of each round trip is played by hand: the test reads
//! the `state` from the address the server would send a browser to, and calls
//! the callback with a code the stand-in knows. The code names the person —
//! `anna`, `bob`, `carol` — and the stand-in answers that person's profile.

use std::sync::Arc;

use axum::extract::Form;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use cartopolis_simulator::{ensure_account, ensure_admin_account, serve, SimConfig, SimHandle};
use serde_json::{json, Value};

/// Who each code is at the stand-ins.
fn person(code: &str) -> Value {
    match code {
        "anna" => json!({"id": "111", "username": "annadv", "global_name": "Anna",
                         "email": "anna@example.org", "verified": true}),
        "bob" => json!({"id": "222", "username": "bobby", "global_name": "Bob",
                        "email": "bob@example.org", "verified": false}),
        "carol" => json!({"id": "333", "username": "carol", "global_name": "Carol"}),
        _ => json!({"id": "999", "username": code}),
    }
}

fn code_of(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer tok-"))
        .unwrap_or_default()
        .to_string()
}

/// Discord's three endpoints and an OpenID server's four, answering from
/// [`person`].
async fn stand_in() -> String {
    async fn token(Form(form): Form<Vec<(String, String)>>) -> Json<Value> {
        let get = |k: &str| form.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        // A server that forgot PKCE or its keys would be refused by the real
        // thing; refuse it here too.
        assert!(get("code_verifier").is_some_and(|v| !v.is_empty()));
        assert_eq!(get("client_secret").as_deref(), Some("sec"));
        Json(json!({"access_token": format!("tok-{}", get("code").unwrap_or_default())}))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let oidc = base.clone() + "/oidc";
    let app = Router::new()
        .route("/discord/api/oauth2/token", post(token))
        .route(
            "/discord/api/users/@me",
            get(|h: HeaderMap| async move { Json(person(&code_of(&h))) }),
        )
        .route(
            "/discord/api/users/@me/guilds",
            get(|h: HeaderMap| async move {
                let code = code_of(&h);
                Json(match code.as_str() {
                    // Bob runs the server; Carol is in it; Anna is not.
                    "bob" => json!([{"id": "g1", "name": "Groningen Fietst", "owner": true, "permissions": "0"}]),
                    "carol" => json!([{"id": "g1", "name": "Groningen Fietst", "owner": false, "permissions": "0"}]),
                    _ => json!([]),
                })
            }),
        )
        .route(
            "/oidc/.well-known/openid-configuration",
            get(move || async move {
                Json(json!({
                    "authorization_endpoint": format!("{oidc}/authorize"),
                    "token_endpoint": format!("{oidc}/token"),
                    "userinfo_endpoint": format!("{oidc}/userinfo"),
                }))
            }),
        )
        .route("/oidc/token", post(token))
        .route(
            "/oidc/userinfo",
            get(|h: HeaderMap| async move {
                let code = code_of(&h);
                Json(json!({"sub": format!("sub-{code}"), "preferred_username": code,
                            "email": format!("{code}@oidc.example"), "email_verified": true}))
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

async fn server() -> (SimHandle, tempfile::TempDir, String) {
    let stand_in = stand_in().await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("signin.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_admin_account(&conn, "root", "adminpass").unwrap();
        ensure_account(&conn, "player", "playerpw").unwrap();
        // The player registered with Anna's address, confirmed.
        conn.execute(
            "UPDATE accounts SET email = 'anna@example.org', email_verified_at = 1 WHERE username = 'player'",
            [],
        )
        .unwrap();
    }
    let config = SimConfig {
        database_path: db.clone(),
        listen: "127.0.0.1:0".into(),
        http_listen: Some("127.0.0.1:0".into()),
        public_base_url: "http://web.test".into(),
        fediverse: false,
        fediverse_allow_private: true,
        signin_test_base: Some(stand_in.clone()),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    (handle, dir, db)
}

fn base(h: &SimHandle) -> String {
    format!("http://{}", h.http_addr.unwrap())
}

/// A request that does not follow redirects: a redirect's `Location` is what
/// several of these are about.
async fn call(
    method: &'static str,
    url: String,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Value, Option<String>) {
    let token = token.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        let agent = ureq::AgentBuilder::new().redirects(0).build();
        let mut req = agent.request(method, &url);
        if let Some(t) = token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        let result = match body {
            Some(b) => req
                .set("Content-Type", "application/json")
                .send_string(&b.to_string()),
            None => req.call(),
        };
        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => panic!("transport error: {e}"),
        };
        let status = resp.status();
        let location = resp.header("location").map(str::to_string);
        let text = resp.into_string().unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::Null),
            location,
        )
    })
    .await
    .unwrap()
}

fn query(url: &str, key: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// Play the provider: take the `state` from the authorize address and come
/// back to the callback as `code`. Answers the ticket `/welcome` would read.
async fn come_back(h: &SimHandle, authorize: &str, provider: &str, code: &str) -> String {
    let state = query(authorize, "state").expect("authorize address carries a state");
    let (status, _, location) = call(
        "GET",
        format!(
            "{}/api/auth/{provider}/callback?code={code}&state={state}",
            base(h)
        ),
        None,
        None,
    )
    .await;
    assert!(
        (300..400).contains(&status),
        "callback redirects, got {status}"
    );
    let location = location.unwrap();
    assert!(
        location.starts_with("http://web.test/welcome?"),
        "{location}"
    );
    query(&location, "ticket")
        .unwrap_or_else(|| panic!("no ticket, error: {:?}", query(&location, "error")))
}

async fn ticket(h: &SimHandle, t: &str) -> Value {
    call(
        "POST",
        format!("{}/api/auth/ticket", base(h)),
        None,
        Some(json!({"ticket": t})),
    )
    .await
    .1
}

/// A signed-out visitor presses the provider's button.
async fn sign_in(h: &SimHandle, provider: &str, code: &str, extra: &str) -> String {
    let (status, _, location) = call(
        "GET",
        format!(
            "{}/api/auth/{provider}/start?return=/buildings{extra}",
            base(h)
        ),
        None,
        None,
    )
    .await;
    assert!(
        (300..400).contains(&status),
        "start redirects, got {status}"
    );
    come_back(h, &location.unwrap(), provider, code).await
}

async fn begin(h: &SimHandle, provider: &str, token: &str, body: Value) -> (u16, Value) {
    let (s, v, _) = call(
        "POST",
        format!("{}/api/auth/{provider}/begin", base(h)),
        Some(token),
        Some(body),
    )
    .await;
    (s, v)
}

async fn login(h: &SimHandle, user: &str, pw: &str) -> String {
    let (_, v, _) = call(
        "POST",
        format!("{}/api/login", base(h)),
        None,
        Some(json!({"username": user, "password": pw})),
    )
    .await;
    v["token"].as_str().unwrap().to_string()
}

/// The operator sets Discord up, tests it and switches it on.
async fn set_up_discord(h: &SimHandle) -> String {
    let admin = login(h, "root", "adminpass").await;
    let url = format!("{}/api/admin/signin/discord", base(h));
    let (s, v, _) = call(
        "PUT",
        url.clone(),
        Some(&admin),
        Some(json!({"client_id": "cid", "client_secret": "sec"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["secret_set"], true);
    assert!(
        v.get("client_secret").is_none(),
        "a secret never comes back"
    );
    let (s, v, _) = call(
        "PUT",
        url.clone(),
        Some(&admin),
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(s, 409, "untested keys stay off: {v}");

    let (s, v) = begin(h, "discord", &admin, json!({"purpose": "test"})).await;
    assert_eq!(s, 200, "{v}");
    let t = come_back(h, v["url"].as_str().unwrap(), "discord", "anna").await;
    assert_eq!(ticket(h, &t).await["kind"], "tested");

    let (s, v, _) = call("PUT", url, Some(&admin), Some(json!({"enabled": true}))).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["live"], true);
    let (_, rules, _) = call("GET", format!("{}/api/register", base(h)), None, None).await;
    assert_eq!(rules["providers"][0]["id"], "discord", "{rules}");
    admin
}

#[tokio::test]
async fn a_new_person_signs_up_with_discord_and_back_in() {
    let (h, _dir, _db) = server().await;
    set_up_discord(&h).await;

    // Bob's Discord address is unverified, so it is no reason to link.
    let t = sign_in(&h, "discord", "bob", "").await;
    let v = ticket(&h, &t).await;
    assert_eq!(v["kind"], "choose_name", "{v}");
    assert_eq!(v["suggestion"], "bobby");
    assert_eq!(v["identity"]["email"], Value::Null);

    let create = |name: &'static str| {
        let (h, t) = (base(&h), t.clone());
        async move {
            call(
                "POST",
                format!("{h}/api/auth/ticket/create"),
                None,
                Some(json!({"ticket": t, "username": name})),
            )
            .await
        }
    };
    let (s, v, _) = create("player").await;
    assert_eq!(s, 409, "a taken name: {v}");
    let (s, v, _) = create("bob").await;
    assert_eq!(s, 200, "{v}");
    let token = v["token"].as_str().unwrap().to_string();
    let (s, _, _) = create("bob2").await;
    assert_eq!(s, 409, "a ticket makes one account");

    // Its sign-ins: Discord, and no password.
    let (_, me, _) = call(
        "GET",
        format!("{}/api/account/signin", base(&h)),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(me["has_password"], false);
    assert_eq!(me["identities"][0]["handle"], "bobby");
    // …which is its only way in.
    let id = me["identities"][0]["id"].as_str().unwrap();
    let (s, _, _) = call(
        "DELETE",
        format!("{}/api/account/identities/{id}", base(&h)),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(s, 409);

    // Signing in again finds the account; the session is handed out once.
    let t = sign_in(&h, "discord", "bob", "").await;
    let v = ticket(&h, &t).await;
    assert_eq!(v["kind"], "signed_in", "{v}");
    assert_eq!(v["username"], "bob");
    assert_eq!(v["return"], "/buildings");
    assert!(v["token"].is_string());
    assert!(ticket(&h, &t).await["token"].is_null());
}

#[tokio::test]
async fn an_address_with_an_account_links_only_with_its_password() {
    let (h, _dir, _db) = server().await;
    set_up_discord(&h).await;
    let t = sign_in(&h, "discord", "anna", "").await;
    let v = ticket(&h, &t).await;
    assert_eq!(v["kind"], "link_existing", "{v}");
    assert_eq!(v["username"], "player");
    let link = |pw: &'static str| {
        let (h, t) = (base(&h), t.clone());
        async move {
            call(
                "POST",
                format!("{h}/api/auth/ticket/link"),
                None,
                Some(json!({"ticket": t, "password": pw})),
            )
            .await
        }
    };
    assert_eq!(link("wrong").await.0, 403);
    let (s, v, _) = link("playerpw").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["username"], "player");
    // From now on Anna's Discord signs in as the player.
    let v = ticket(&h, &sign_in(&h, "discord", "anna", "").await).await;
    assert_eq!(v["kind"], "signed_in");
    assert_eq!(v["username"], "player");
}

#[tokio::test]
async fn an_account_without_a_password_proves_itself_through_its_provider() {
    let (h, _dir, _db) = server().await;
    set_up_discord(&h).await;
    let t = sign_in(&h, "discord", "bob", "").await;
    let (_, v, _) = call(
        "POST",
        format!("{}/api/auth/ticket/create", base(&h)),
        None,
        Some(json!({"ticket": t, "username": "bob"})),
    )
    .await;
    let token = v["token"].as_str().unwrap().to_string();
    let make = || {
        let (h, token) = (base(&h), token.clone());
        async move {
            call(
                "POST",
                format!("{h}/api/tokens"),
                Some(&token),
                Some(json!({"label": "bot"})),
            )
            .await
        }
    };
    let (s, v, _) = make().await;
    assert_eq!(s, 403);
    assert_eq!(v["reauth"], true, "{v}");

    // Somebody else's Discord does not count.
    let (_, v) = begin(
        &h,
        "discord",
        &token,
        json!({"purpose": "reauth", "return": "/tokens"}),
    )
    .await;
    let t = come_back(&h, v["url"].as_str().unwrap(), "discord", "carol").await;
    assert_eq!(ticket(&h, &t).await["kind"], "refused");
    assert_eq!(make().await.0, 403);

    let (_, v) = begin(
        &h,
        "discord",
        &token,
        json!({"purpose": "reauth", "return": "/tokens"}),
    )
    .await;
    let t = come_back(&h, v["url"].as_str().unwrap(), "discord", "bob").await;
    let v = ticket(&h, &t).await;
    assert_eq!(v["kind"], "reauthed");
    assert_eq!(v["return"], "/tokens");
    let (s, v, _) = make().await;
    assert_eq!(s, 200, "{v}");

    // And may now add a password.
    let (s, _, _) = call(
        "POST",
        format!("{}/api/account/password", base(&h)),
        Some(&token),
        Some(json!({"password": "a long password"})),
    )
    .await;
    assert_eq!(s, 204);
    assert!(!login(&h, "bob", "a long password").await.is_empty());
}

#[tokio::test]
async fn a_linked_discord_server_brings_its_members_to_the_community() {
    let (h, _dir, db) = server().await;
    set_up_discord(&h).await;
    // Bob signs up and owns a community.
    let t = sign_in(&h, "discord", "bob", "").await;
    let (_, v, _) = call(
        "POST",
        format!("{}/api/auth/ticket/create", base(&h)),
        None,
        Some(json!({"ticket": t, "username": "bob"})),
    )
    .await;
    let bob = v["token"].as_str().unwrap().to_string();
    let bob_id = v["principal"].as_str().unwrap().to_string();
    let community = uuid::Uuid::new_v4().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        conn.execute(
            "INSERT INTO communities (id, name, about, join_rule, listed, members_start_topics, created_by, created_ms)
             VALUES (?1, 'Fietsers', '', 2, 1, 1, ?2, 0)",
            [&community, &bob_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO community_members (community, account, role, joined_ms) VALUES (?1, ?2, 0, 0)",
            [&community, &bob_id],
        )
        .unwrap();
    }

    // A player who is no moderator may not link it.
    let player = login(&h, "player", "playerpw").await;
    let (s, _) = begin(
        &h,
        "discord",
        &player,
        json!({"purpose": "community", "community": community}),
    )
    .await;
    assert_eq!(s, 403);

    let (s, v) = begin(
        &h,
        "discord",
        &bob,
        json!({"purpose": "community", "community": community}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let t = come_back(&h, v["url"].as_str().unwrap(), "discord", "bob").await;
    let v = ticket(&h, &t).await;
    assert_eq!(v["kind"], "community_groups", "{v}");
    assert_eq!(v["groups"][0]["group_id"], "g1");
    let (s, v, _) = call(
        "POST",
        format!("{}/api/auth/ticket/community", base(&h)),
        Some(&bob),
        Some(json!({"ticket": t, "group_id": "g1"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let (_, v, _) = call(
        "GET",
        format!("{}/api/communities/{community}/links", base(&h)),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(v["links"][0]["group_name"], "Groningen Fietst", "{v}");

    // Carol is in that Discord server: signing up offers the community, even
    // though it is invite-only, and joining it is part of making the account.
    let t = sign_in(&h, "discord", "carol", "").await;
    let v = ticket(&h, &t).await;
    assert_eq!(v["kind"], "choose_name", "{v}");
    assert_eq!(v["offers"][0]["community"], community.as_str(), "{v}");
    let (s, v, _) = call(
        "POST",
        format!("{}/api/auth/ticket/create", base(&h)),
        None,
        Some(json!({"ticket": t, "username": "carol", "join": [community]})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["joined"][0], community.as_str());
    let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
    let members: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM community_members WHERE community = ?1",
            [&community],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(members, 2);
}

#[tokio::test]
async fn the_app_collects_its_own_session_by_code() {
    let (h, _dir, _db) = server().await;
    set_up_discord(&h).await;
    let (_, pair, _) = call(
        "POST",
        format!("{}/api/auth/app", base(&h)),
        None,
        Some(json!({})),
    )
    .await;
    let (code, secret) = (
        pair["code"].as_str().unwrap(),
        pair["secret"].as_str().unwrap(),
    );
    let poll = || {
        let url = format!("{}/api/auth/app/{code}?secret={secret}", base(&h));
        async move { call("GET", url, None, None).await }
    };
    assert_eq!(poll().await.1["status"], "waiting");
    let t = sign_in(&h, "discord", "bob", &format!("&app={code}")).await;
    let (_, v, _) = call(
        "POST",
        format!("{}/api/auth/ticket/create", base(&h)),
        None,
        Some(json!({"ticket": t, "username": "bob"})),
    )
    .await;
    assert_eq!(v["app_code"], code);
    let browser = v["token"].as_str().unwrap().to_string();
    let (_, done, _) = poll().await;
    assert_eq!(done["status"], "done", "{done}");
    assert_eq!(done["username"], "bob");
    assert_ne!(
        done["token"].as_str().unwrap(),
        browser,
        "the app's own session"
    );
    assert_eq!(poll().await.0, 404, "handed over once");
}

#[tokio::test]
async fn a_communitys_own_openid_server_is_configuration() {
    let (h, _dir, _db) = server().await;
    let admin = login(&h, "root", "adminpass").await;
    let issuer = query_issuer(&h).await;
    let (s, v, _) = call(
        "PUT",
        format!("{}/api/admin/signin/oidc-buurt", base(&h)),
        Some(&admin),
        Some(json!({"name": "Buurt", "issuer": issuer, "client_id": "c", "client_secret": "sec"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["custom"], true);
    let (_, v) = begin(&h, "oidc-buurt", &admin, json!({"purpose": "test"})).await;
    let t = come_back(&h, v["url"].as_str().unwrap(), "oidc-buurt", "dave").await;
    assert_eq!(ticket(&h, &t).await["kind"], "tested");
    call(
        "PUT",
        format!("{}/api/admin/signin/oidc-buurt", base(&h)),
        Some(&admin),
        Some(json!({"enabled": true})),
    )
    .await;
    let v = ticket(&h, &sign_in(&h, "oidc-buurt", "dave", "").await).await;
    assert_eq!(v["kind"], "choose_name", "{v}");
    assert_eq!(v["identity"]["email"], "dave@oidc.example");
    assert_eq!(v["suggestion"], "dave");
    // A player may not touch any of it.
    let player = login(&h, "player", "playerpw").await;
    let (s, _, _) = call(
        "GET",
        format!("{}/api/admin/signin", base(&h)),
        Some(&player),
        None,
    )
    .await;
    assert_eq!(s, 403);
}

/// The stand-in's OpenID issuer, read back from the server's own config.
async fn query_issuer(_h: &SimHandle) -> String {
    // The stand-in is started per server; its address is the one the config
    // names, which `server()` does not return. Start a second one: any
    // stand-in answers the same people.
    let base = stand_in().await;
    format!("{base}/oidc")
}
