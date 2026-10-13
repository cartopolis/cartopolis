//! Two simulators on one machine, federating with each other over HTTP the way
//! two fediverse servers do (`docs/notes/fediverse.md`): one community follows
//! another, a post crosses with its place, a reply comes back, and a deletion
//! follows it. Nothing here is mocked — both ends sign and verify, fetch actors
//! and queue deliveries — except that `fediverse_allow_private` lets them reach
//! each other on loopback over plain `http`.

use std::sync::Arc;
use std::time::Duration;

use cartopolis_core::point::PointView;
use cartopolis_core::posts::{NewPost, PostDto, PostPlace, PostThread, PublicInfo};
use cartopolis_core::{CommunitySettings, JoinRule};
use cartopolis_simulator::{communities, ensure_account, serve, SimConfig, SimHandle};
use serde_json::{json, Value};
use uuid::Uuid;

struct Server {
    _handle: SimHandle,
    _dir: tempfile::TempDir,
    base: String,
    community: Uuid,
}

/// A port nobody is using, so the server can be told its own public address
/// before it starts.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn server(host: &str, owner: &str, members: &[&str], community: &str) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("world.db").to_str().unwrap().to_string();
    let port = free_port();
    let community = {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        let owner_id = ensure_account(&conn, owner, "pw").unwrap().to_string();
        let settings = CommunitySettings {
            name: community.into(),
            about: "neighbours".into(),
            join: JoinRule::Open,
            listed: true,
            members_start_topics: true,
            picture: None,
            place: None,
        };
        let id = communities::create(&conn, &owner_id, &settings, 1).unwrap();
        for m in members {
            let m = ensure_account(&conn, m, "pw").unwrap().to_string();
            communities::join(&conn, &m, id, "", 2).unwrap();
        }
        id
    };
    let base = format!("http://{host}:{port}");
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        http_listen: Some(format!("127.0.0.1:{port}")),
        asset_dir: dir.path().join("assets").to_str().unwrap().to_string(),
        public_base_url: base.clone(),
        fediverse: true,
        fediverse_allow_private: true,
        fediverse_blocklists: Vec::new(),
        ..SimConfig::default()
    };
    Server {
        _handle: serve(Arc::new(config)).await.unwrap(),
        _dir: dir,
        base,
        community,
    }
}

/// One HTTP call: (status, JSON body).
async fn call(
    method: &'static str,
    url: String,
    token: Option<String>,
    body: Option<Value>,
) -> (u16, Value) {
    tokio::task::spawn_blocking(move || {
        let mut req = ureq::request(method, &url);
        if let Some(t) = token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        let res = match body {
            Some(b) => req
                .set("Content-Type", "application/json")
                .send_string(&b.to_string()),
            None => req.call(),
        };
        let json = |r: ureq::Response| {
            serde_json::from_str(&r.into_string().unwrap_or_default()).unwrap_or(Value::Null)
        };
        match res {
            Ok(r) => (r.status(), json(r)),
            Err(ureq::Error::Status(code, r)) => (code, json(r)),
            Err(e) => panic!("{url}: {e}"),
        }
    })
    .await
    .unwrap()
}

async fn login(s: &Server, username: &str) -> String {
    let (status, body) = call(
        "POST",
        format!("{}/api/login", s.base),
        None,
        Some(json!({ "username": username, "password": "pw" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["token"].as_str().unwrap().to_string()
}

/// Ask until `check` holds, for up to ten seconds: deliveries are queued.
async fn eventually<T>(what: &str, mut check: impl AsyncFnMut() -> Option<T>) -> T {
    for _ in 0..100 {
        if let Some(v) = check().await {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never happened: {what}");
}

async fn switch_on(s: &Server, token: &str, handle: &str) {
    let (status, body) = call(
        "PUT",
        format!("{}/api/communities/{}/public", s.base, s.community),
        Some(token.into()),
        Some(json!({ "enabled": true, "handle": handle, "who_posts": "members" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_post_crosses_to_a_following_server_and_a_reply_comes_back() {
    let a = server("127.0.0.1", "karim", &["sanne"], "Buurt Oosterpoort").await;
    let b = server("localhost", "bob", &[], "Bob's street").await;
    let karim = login(&a, "karim").await;
    let sanne = login(&a, "sanne").await;
    let bob = login(&b, "bob").await;
    switch_on(&a, &karim, "oost").await;
    switch_on(&b, &bob, "bobs").await;

    // Nobody who has not posted is findable from outside; the community is.
    let host_a = a.base.trim_start_matches("http://").to_string();
    let (status, _) = call(
        "GET",
        format!(
            "{}/.well-known/webfinger?resource=acct:sanne@{host_a}",
            a.base
        ),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);
    let (status, finger) = call(
        "GET",
        format!(
            "{}/.well-known/webfinger?resource=acct:oost@{host_a}",
            a.base
        ),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "{finger}");
    // The server itself answers as Mastodon's does, which is what a server in
    // authorized-fetch mode looks up before it believes our signed requests.
    let (status, finger) = call(
        "GET",
        format!(
            "{}/.well-known/webfinger?resource=acct:{host_a}@{host_a}",
            a.base
        ),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "{finger}");
    assert_eq!(finger["links"][0]["href"], format!("{}/ap/actor", a.base));

    // B's community follows A's, and A says yes.
    let (status, info) = call(
        "POST",
        format!("{}/api/communities/{}/follows", b.base, b.community),
        Some(bob.clone()),
        Some(json!({ "address": format!("oost@{host_a}") })),
    )
    .await;
    assert_eq!(status, 200, "{info}");
    let follows_url = format!("{}/api/communities/{}/public", b.base, b.community);
    eventually("B's follow of A is accepted", async || {
        let (_, v) = call("GET", follows_url.clone(), Some(bob.clone()), None).await;
        let info: PublicInfo = serde_json::from_value(v).ok()?;
        info.follows.first().filter(|f| f.accepted).cloned()
    })
    .await;
    let (_, v) = call(
        "GET",
        format!("{}/api/communities/{}/public", a.base, a.community),
        None,
        None,
    )
    .await;
    assert_eq!(v["followers"], 1, "{v}");

    // Sanne posts on A, with a place.
    let place = PostPlace {
        latitude: 53.2105,
        longitude: 6.5801,
        name: Some("Meeuwerderweg".into()),
        view: Some(PointView {
            altitude: 120.0,
            pitch: -30.0,
            yaw: 15.0,
        }),
    };
    let new = NewPost {
        text: "Plant swap on the corner this Saturday".into(),
        picture: None,
        place: place.clone(),
    };
    let (status, posted) = call(
        "POST",
        format!("{}/api/communities/{}/posts", a.base, a.community),
        Some(sanne.clone()),
        Some(serde_json::to_value(&new).unwrap()),
    )
    .await;
    assert_eq!(status, 201, "{posted}");
    let posted: PostDto = serde_json::from_value(posted).unwrap();

    // It arrives on B, from Sanne, through A's community, with its place.
    let feed_b = format!("{}/api/communities/{}/posts", b.base, b.community);
    let arrived: PostDto = eventually("the post reaches B", async || {
        let (_, v) = call("GET", feed_b.clone(), Some(bob.clone()), None).await;
        let feed: Vec<PostDto> = serde_json::from_value(v).ok()?;
        feed.into_iter().next()
    })
    .await;
    assert_eq!(arrived.text, new.text);
    assert_eq!(arrived.author.address, "sanne@127.0.0.1");
    assert_eq!(arrived.via.as_deref(), Some("oost@127.0.0.1"));
    assert!(!arrived.author.local);
    let p = arrived.place.clone().unwrap();
    assert!((p.latitude - place.latitude).abs() < 1e-9);
    assert_eq!(p.view.unwrap().yaw, 15.0);

    // Now Sanne is findable: she has posted.
    let (status, _) = call(
        "GET",
        format!(
            "{}/.well-known/webfinger?resource=acct:sanne@{host_a}",
            a.base
        ),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);

    // Bob replies on B; the reply lands under Sanne's post on A.
    let (status, reply) = call(
        "POST",
        format!("{}/api/posts/{}/replies", b.base, arrived.id),
        Some(bob.clone()),
        Some(json!({ "text": "I'll bring monstera cuttings" })),
    )
    .await;
    assert_eq!(status, 201, "{reply}");
    let thread_a = format!("{}/api/posts/{}", a.base, posted.id);
    let thread: PostThread = eventually("the reply reaches A", async || {
        let (_, v) = call("GET", thread_a.clone(), Some(sanne.clone()), None).await;
        let t: PostThread = serde_json::from_value(v).ok()?;
        (!t.replies.is_empty()).then_some(t)
    })
    .await;
    assert_eq!(thread.replies[0].text, "I'll bring monstera cuttings");
    assert_eq!(thread.replies[0].author.address, "bob@localhost");
    assert_eq!(thread.post.replies, 1);

    // Sanne deletes the post; B's copy goes too.
    let (status, _) = call(
        "DELETE",
        format!("{}/api/posts/{}", a.base, posted.id),
        Some(sanne.clone()),
        None,
    )
    .await;
    assert_eq!(status, 204);
    eventually("the deletion reaches B", async || {
        let (_, v) = call("GET", feed_b.clone(), Some(bob.clone()), None).await;
        let feed: Vec<PostDto> = serde_json::from_value(v).ok()?;
        feed.is_empty().then_some(())
    })
    .await;

    // The served post is gone, and an unsigned delivery is refused.
    let (status, _) = call("GET", format!("{}/ap/p/{}", a.base, posted.id), None, None).await;
    assert_eq!(status, 404);
    let (status, _) = call(
        "POST",
        format!("{}/ap/inbox", a.base),
        None,
        Some(json!({ "type": "Create", "actor": "https://evil.example/u/x" })),
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocked_server_is_not_heard() {
    let a = server("127.0.0.1", "karim", &[], "Buurt").await;
    let b = server("localhost", "bob", &[], "Street").await;
    let karim = login(&a, "karim").await;
    let bob = login(&b, "bob").await;
    switch_on(&a, &karim, "buurt").await;
    switch_on(&b, &bob, "street").await;
    // A blocks B's server, written straight into the table the shared list
    // and the operator's own blocks both live in.
    {
        let conn = rusqlite::Connection::open(a._dir.path().join("world.db")).unwrap();
        conn.execute(
            "INSERT INTO fedi_server_blocks (host, source, reason, added_ms) VALUES ('localhost', 'admin', '', 0)",
            [],
        )
        .unwrap();
    }
    let host_a = a.base.trim_start_matches("http://").to_string();
    let (status, _) = call(
        "POST",
        format!("{}/api/communities/{}/follows", b.base, b.community),
        Some(bob.clone()),
        Some(json!({ "address": format!("buurt@{host_a}") })),
    )
    .await;
    assert_eq!(status, 200);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (_, v) = call(
        "GET",
        format!("{}/api/communities/{}/public", a.base, a.community),
        None,
        None,
    )
    .await;
    assert_eq!(v["followers"], 0, "{v}");
}
