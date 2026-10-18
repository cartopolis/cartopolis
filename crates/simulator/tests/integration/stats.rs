//! Usage statistics end to end (`crate::stats`): a page view counted from the
//! beacon, a session counted from a real sign-in with the app named on the
//! `/ws` address, and who may read the numbers.

use std::sync::Arc;

use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, NetMessage, PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, ensure_admin_account, serve, SimConfig, SimHandle};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

async fn server() -> (SimHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("world.db").to_str().unwrap().to_string();
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
    (serve(Arc::new(config)).await.unwrap(), dir)
}

fn base(handle: &SimHandle) -> String {
    format!("http://{}", handle.http_addr.unwrap())
}

async fn call(
    method: &'static str,
    url: String,
    token: Option<String>,
    user_agent: &'static str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    tokio::task::spawn_blocking(move || {
        let mut req = ureq::request(method, &url).set("User-Agent", user_agent);
        if let Some(token) = token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        let result = match body {
            Some(json) => req
                .set("Content-Type", "application/json")
                .send_string(&json.to_string()),
            None => req.call(),
        };
        let response = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => panic!("transport error: {e}"),
        };
        let status = response.status();
        let text = response.into_string().unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
        )
    })
    .await
    .unwrap()
}

async fn token(handle: &SimHandle, path: &str, user: &str, pass: &str) -> String {
    let (status, body) = call(
        "POST",
        format!("{}{path}", base(handle)),
        None,
        "test",
        Some(serde_json::json!({"username": user, "password": pass})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["token"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn page_views_count_visitors_once_and_bots_apart() {
    let (handle, _dir) = server().await;
    let hit = serde_json::json!({"site": "web", "path": "/building/0014100010938173", "referrer": "https://mastodon.social/@x/1", "width": 390});
    for ua in [
        "Mozilla/5.0 Firefox",
        "Mozilla/5.0 Firefox",
        "Mozilla/5.0 Chrome",
    ] {
        let (status, _) = call(
            "POST",
            format!("{}/api/stats/hit", base(&handle)),
            None,
            ua,
            Some(hit.clone()),
        )
        .await;
        assert_eq!(status, 204);
    }
    call(
        "POST",
        format!("{}/api/stats/hit", base(&handle)),
        None,
        "Googlebot/2.1",
        Some(hit.clone()),
    )
    .await;
    // A site that is not ours is ignored, and still answered with nothing.
    let (status, _) = call(
        "POST",
        format!("{}/api/stats/hit", base(&handle)),
        None,
        "Mozilla/5.0",
        Some(serde_json::json!({"site": "elsewhere", "path": "/"})),
    )
    .await;
    assert_eq!(status, 204);
    handle.stats.flush().await.unwrap();

    let admin = token(&handle, "/api/admin/login", "root", "adminpass").await;
    let (status, web) = call(
        "GET",
        format!("{}/api/admin/stats/web?days=7", base(&handle)),
        Some(admin),
        "test",
        None,
    )
    .await;
    assert_eq!(status, 200, "{web}");
    assert_eq!(web["visitors"], 2, "{web}");
    assert_eq!(web["views"], 3, "{web}");
    assert_eq!(web["bots"], 1, "{web}");
    assert_eq!(web["pages"][0]["value"], "/building/:id");
    assert_eq!(web["sources"][0]["value"], "mastodon.social");
    assert_eq!(web["devices"][0]["value"], "phone");
    // No country table in a test: the visitors are counted as unknown.
    assert_eq!(web["countries"][0]["value"], "unknown");
}

/// The about site posts with `navigator.sendBeacon`, which sends `text/plain`;
/// it must count the same as JSON.
#[tokio::test(flavor = "multi_thread")]
async fn a_beacon_from_the_about_site_counts_as_plain_text() {
    let (handle, _dir) = server().await;
    let url = format!("{}/api/stats/hit", base(&handle));
    let status = tokio::task::spawn_blocking(move || {
        ureq::post(&url)
            .set("User-Agent", "Mozilla/5.0 Firefox")
            .set("Content-Type", "text/plain;charset=UTF-8")
            .send_string(r#"{"site":"about","path":"/","referrer":"","width":1280}"#)
            .map(|r| r.status())
            .unwrap_or(0)
    })
    .await
    .unwrap();
    assert_eq!(status, 204);
    handle.stats.flush().await.unwrap();
    let admin = token(&handle, "/api/admin/login", "root", "adminpass").await;
    let (_, web) = call(
        "GET",
        format!("{}/api/admin/stats/web?site=about", base(&handle)),
        Some(admin),
        "test",
        None,
    )
    .await;
    assert_eq!(web["visitors"], 1, "{web}");
    assert_eq!(web["pages"][0]["value"], "about /", "{web}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_in_app_is_a_session_with_its_name_and_build() {
    let (handle, _dir) = server().await;
    let player = token(&handle, "/api/login", "player", "playerpw").await;
    let url = format!(
        "ws://{}/ws?client=android&build=410d46f4",
        handle.http_addr.unwrap()
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let hello = encode_app_frame(&NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth: AuthCredential::Token(player),
    })
    .unwrap();
    socket.send(Message::Binary(hello.into())).await.unwrap();
    loop {
        if let Message::Binary(b) = socket.next().await.unwrap().unwrap() {
            assert!(matches!(
                decode_app_frame(&b).unwrap(),
                NetMessage::ServerHelloAck { .. }
            ));
            break;
        }
    }
    assert_eq!(handle.stats.online(), 1);
    handle.stats.flush().await.unwrap();

    let admin = token(&handle, "/api/admin/login", "root", "adminpass").await;
    let (status, o) = call(
        "GET",
        format!("{}/api/admin/stats/overview", base(&handle)),
        Some(admin.clone()),
        "test",
        None,
    )
    .await;
    assert_eq!(status, 200, "{o}");
    assert_eq!(o["online_now"], 1, "{o}");
    assert_eq!(o["active_today"], 1, "{o}");
    let today = o["daily"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(today["android"], 1, "{today}");
    let (_, w) = call(
        "GET",
        format!("{}/api/admin/stats/world", base(&handle)),
        Some(admin),
        "test",
        None,
    )
    .await;
    assert_eq!(w["apps"][0]["client"], "android", "{w}");
    assert_eq!(w["apps"][0]["build"], "410d46f4", "{w}");
    assert_eq!(w["sessions"], 1, "{w}");

    drop(socket);
    // The session ends with its connection.
    for _ in 0..50 {
        if handle.stats.online() == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(handle.stats.online(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_sign_in_is_counted_without_an_account() {
    let (handle, _dir) = server().await;
    let url = format!("ws://{}/ws?client=desktop", handle.http_addr.unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let hello = encode_app_frame(&NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth: AuthCredential::Token("not-a-token".into()),
    })
    .unwrap();
    socket.send(Message::Binary(hello.into())).await.unwrap();
    let _ = socket.next().await;
    for _ in 0..50 {
        handle.stats.flush().await.unwrap();
        let admin = token(&handle, "/api/admin/login", "root", "adminpass").await;
        let (_, w) = call(
            "GET",
            format!("{}/api/admin/stats/world", base(&handle)),
            Some(admin),
            "test",
            None,
        )
        .await;
        if let Some(r) = w["refused"].as_array().and_then(|r| r.first()) {
            assert_eq!(r["reason"], "sign_in");
            assert_eq!(r["client"], "desktop");
            assert_eq!(r["devices"], 1);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the refusal was never counted");
}

/// The atlas web UI's hand-written types for these answers
/// (`crates/atlas/web/src/types.ts`), read against what the server sends: a
/// renamed field otherwise arrives in the page as a number that reads
/// `undefined`.
#[tokio::test(flavor = "multi_thread")]
async fn the_web_pages_types_match_what_the_server_sends() {
    const WEB_TYPES: &str = include_str!("../../../atlas/web/src/types.ts");
    let fields = |name: &str| {
        let mut f = cartopolis_core::ts_mirror::type_fields(WEB_TYPES, name)
            .unwrap_or_else(|| panic!("`{name}` is not declared in types.ts"));
        f.sort();
        f
    };
    let keys = |v: &serde_json::Value| {
        let mut k: Vec<String> = v.as_object().expect("an object").keys().cloned().collect();
        k.sort();
        k
    };

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("world.db").to_str().unwrap().to_string();
    let community = {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_admin_account(&conn, "root", "adminpass").unwrap();
        let owner = ensure_account(&conn, "owner", "ownerpw")
            .unwrap()
            .to_string();
        let member = ensure_account(&conn, "member", "memberpw")
            .unwrap()
            .to_string();
        let settings = cartopolis_core::CommunitySettings {
            name: "Oosterpoort".into(),
            about: String::new(),
            join: cartopolis_core::JoinRule::Open,
            listed: true,
            members_start_topics: true,
            picture: None,
            place: None,
        };
        let id = cartopolis_simulator::communities::create(&conn, &owner, &settings, 1).unwrap();
        cartopolis_simulator::communities::join(&conn, &member, id, "", 2).unwrap();
        id
    };
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        http_listen: Some("127.0.0.1:0".into()),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();

    // Somebody outside the community opens its public page: a visitor.
    call(
        "GET",
        format!("{}/api/communities/{community}/public", base(&handle)),
        None,
        "Mozilla/5.0 Firefox",
        None,
    )
    .await;
    handle.stats.flush().await.unwrap();

    let admin = token(&handle, "/api/admin/login", "root", "adminpass").await;
    for (page, name) in [
        ("overview", "StatsOverview"),
        ("world", "StatsWorld"),
        ("web", "StatsWeb"),
        ("places", "StatsPlaces"),
    ] {
        let (status, body) = call(
            "GET",
            format!("{}/api/admin/stats/{page}", base(&handle)),
            Some(admin.clone()),
            "test",
            None,
        )
        .await;
        assert_eq!(status, 200, "{page}: {body}");
        assert_eq!(
            fields(name),
            keys(&body),
            "types.ts's {name} has drifted from stats::api"
        );
    }

    let owner = token(&handle, "/api/login", "owner", "ownerpw").await;
    let url = format!("{}/api/communities/{community}/stats", base(&handle));
    let (status, body) = call("GET", url.clone(), Some(owner), "test", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        fields("CommunityStats"),
        keys(&body),
        "types.ts's CommunityStats has drifted from stats::api"
    );
    assert_eq!(body["members"], 2, "{body}");
    assert_eq!(body["visitors"], 1, "{body}");
    // Fewer than 5 active members is a gap, not a number.
    assert!(body["active_members"].is_null(), "{body}");

    // A member who does not moderate is refused.
    let member = token(&handle, "/api/login", "member", "memberpw").await;
    let (status, _) = call("GET", url, Some(member), "test", None).await;
    assert_eq!(status, 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_accounts_allowed_to_read_them_see_the_numbers() {
    let (handle, _dir) = server().await;
    let player = token(&handle, "/api/login", "player", "playerpw").await;
    for page in ["overview", "world", "web", "places"] {
        let (status, _) = call(
            "GET",
            format!("{}/api/admin/stats/{page}", base(&handle)),
            Some(player.clone()),
            "test",
            None,
        )
        .await;
        assert_eq!(status, 403, "{page}");
    }
    let (status, _) = call(
        "GET",
        format!(
            "{}/api/communities/{}/stats",
            base(&handle),
            uuid::Uuid::new_v4()
        ),
        Some(player),
        "test",
        None,
    )
    .await;
    assert_eq!(status, 403);
}
