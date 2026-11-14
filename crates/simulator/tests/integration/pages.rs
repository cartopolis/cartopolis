//! Content pages end to end (`crate::pages`, `docs/mockups/cms`): an agent
//! writes and publishes a page over MCP with an admin's bearer, a reader fetches
//! it with no sign-in, the shell carries the page's own head, and a player may
//! neither read the drafts nor publish.

use std::sync::Arc;

use cartopolis_simulator::{ensure_account, ensure_admin_account, serve, SimConfig, SimHandle};
use serde_json::{json, Value};

async fn server() -> (SimHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("world.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_admin_account(&conn, "root", "adminpass").unwrap();
        ensure_account(&conn, "player", "playerpw").unwrap();
    }
    let web = dir.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<html>the chat</html>").unwrap();
    std::fs::write(
        web.join("pages.html"),
        "<html lang=\"en\"><head><!--cms-head--><title>Cartopolis</title><!--/cms-head--></head><body data-cms></body></html>",
    )
    .unwrap();
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        http_listen: Some("127.0.0.1:0".into()),
        web_root: Some(web.to_str().unwrap().into()),
        public_base_url: "https://example.org".into(),
        ..SimConfig::default()
    };
    (serve(Arc::new(config)).await.unwrap(), dir)
}

fn base(handle: &SimHandle) -> String {
    format!("http://{}", handle.http_addr.unwrap())
}

/// Status, body text and its `Location`, following no redirect.
async fn raw(
    method: &'static str,
    url: String,
    token: Option<String>,
    body: Option<Value>,
    accept_language: Option<&'static str>,
) -> (u16, String, Option<String>) {
    tokio::task::spawn_blocking(move || {
        let agent = ureq::AgentBuilder::new().redirects(0).build();
        let mut req = agent.request(method, &url);
        if let Some(token) = token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        if let Some(l) = accept_language {
            req = req.set("Accept-Language", l);
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
        let location = response.header("Location").map(str::to_string);
        (status, response.into_string().unwrap_or_default(), location)
    })
    .await
    .unwrap()
}

async fn call(
    method: &'static str,
    url: String,
    token: Option<String>,
    body: Option<Value>,
) -> (u16, Value) {
    let (status, text, _) = raw(method, url, token, body, None).await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn token(handle: &SimHandle, path: &str, user: &str, pass: &str) -> String {
    let (status, body) = call(
        "POST",
        format!("{}{path}", base(handle)),
        None,
        Some(json!({"username": user, "password": pass})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["token"].as_str().unwrap().to_string()
}

/// One MCP tool call; the text it answered and whether it was an error.
async fn tool(handle: &SimHandle, token: &str, name: &str, arguments: Value) -> (String, bool) {
    let (status, body) = call(
        "POST",
        format!("{}/mcp", base(handle)),
        Some(token.to_string()),
        Some(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": name, "arguments": arguments}})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["id"], 7);
    let result = &body["result"];
    (
        result["content"][0]["text"].as_str().unwrap().to_string(),
        result["isError"].as_bool().unwrap(),
    )
}

const JSX: &str = r#"<Page title={$t('title')} description={$t('description')}>
  <Hero heading={$t('hero.heading')}>
    <Text lead>{$t('hero.lead')}</Text>
  </Hero>
</Page>"#;

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_writes_and_publishes_a_page_readers_get_it() {
    let (handle, _dir) = server().await;
    let admin = token(&handle, "/api/admin/login", "root", "adminpass").await;

    // The handshake and the list of tools.
    let (status, init) = call(
        "POST",
        format!("{}/mcp", base(&handle)),
        Some(admin.clone()),
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    let (status, _) = call(
        "POST",
        format!("{}/mcp", base(&handle)),
        Some(admin.clone()),
        Some(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})),
    )
    .await;
    assert_eq!(status, 202);
    let (_, list) = call(
        "POST",
        format!("{}/mcp", base(&handle)),
        Some(admin.clone()),
        Some(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})),
    )
    .await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"put_page") && names.contains(&"publish"),
        "{names:?}"
    );

    // Bare text is refused with where it is.
    let (text, error) = tool(&handle, &admin, "put_page", json!({"slug": "over", "jsx": "<Page title={$t('t')} description={$t('d')}>\n<Text>Hallo</Text></Page>"})).await;
    assert!(error && text.contains("2:7 bare text"), "{text}");

    let (text, error) = tool(
        &handle,
        &admin,
        "put_page",
        json!({"slug": "over", "jsx": JSX, "texts": {"nl": {"title": "Over **Cartopolis**", "description": "Een plek \"voor\" buren.", "hero.heading": "Jouw stad", "hero.lead": "Samen."}}}),
    )
    .await;
    assert!(
        !error && text.contains("Not live yet") && text.contains("en: missing"),
        "{text}"
    );

    // Not live: readers get a 404, the shell too.
    let (status, _) = call(
        "GET",
        format!("{}/api/pages/over?lang=nl", base(&handle)),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);

    let (text, error) = tool(
        &handle,
        &admin,
        "set_texts",
        json!({"slug": "over", "lang": "en", "texts": {"title": "About Cartopolis"}}),
    )
    .await;
    assert!(!error, "{text}");
    let (text, _) = tool(
        &handle,
        &admin,
        "preview",
        json!({"slug": "over", "lang": "en"}),
    )
    .await;
    assert!(
        text.contains("title=\"About Cartopolis\"")
            && text.contains("[nl, no en yet]")
            && text.contains("https://example.org/en/over?draft"),
        "{text}"
    );

    let (text, error) = tool(&handle, &admin, "publish", json!({"slug": "over"})).await;
    assert!(!error && text.contains("Revision 1 is live"), "{text}");

    // A reader, signed in to nothing.
    let (status, page) = call(
        "GET",
        format!("{}/api/pages/over?lang=en", base(&handle)),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["revision"], 1);
    assert_eq!(
        page["texts"]["title"],
        json!({"text": "About Cartopolis", "lang": "en"})
    );
    assert_eq!(
        page["texts"]["hero.heading"],
        json!({"text": "Jouw stad", "lang": "nl"})
    );
    assert_eq!(page["tree"]["c"], "Page");

    // The shell: the page's head, escaped, in its language; the chat elsewhere.
    let (status, html, _) = raw(
        "GET",
        format!("{}/nl/over", base(&handle)),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(html.starts_with("<html lang=\"nl\">"), "{html}");
    assert!(
        html.contains("<title>Over Cartopolis · Cartopolis</title>"),
        "{html}"
    );
    assert!(
        html.contains("content=\"Een plek &quot;voor&quot; buren.\""),
        "{html}"
    );
    assert!(
        html.contains("hreflang=\"en\" href=\"https://example.org/en/over\""),
        "{html}"
    );
    assert!(!html.contains("noindex"), "{html}");
    let (status, html, _) = raw(
        "GET",
        format!("{}/nl/elders", base(&handle)),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);
    assert!(html.contains("noindex"));
    let (_, html, _) = raw(
        "GET",
        format!("{}/c/groningen", base(&handle)),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(html, "<html>the chat</html>");
    let (status, _, location) = raw(
        "GET",
        format!("{}/p/over", base(&handle)),
        None,
        None,
        Some("en-GB,en;q=0.9"),
    )
    .await;
    assert!((300..400).contains(&status));
    assert_eq!(location.as_deref(), Some("/en/over"));

    // A player may read pages, not drafts, and may not write over MCP.
    let player = token(&handle, "/api/login", "player", "playerpw").await;
    let (status, _) = call(
        "GET",
        format!("{}/api/admin/pages", base(&handle)),
        Some(player.clone()),
        None,
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = call(
        "POST",
        format!("{}/mcp", base(&handle)),
        Some(player),
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})),
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = call(
        "POST",
        format!("{}/mcp", base(&handle)),
        None,
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})),
    )
    .await;
    assert_eq!(status, 401);

    // Taking it down.
    let (_, error) = tool(&handle, &admin, "delete_page", json!({"slug": "over"})).await;
    assert!(error);
    tool(&handle, &admin, "unpublish", json!({"slug": "over"})).await;
    let (status, _) = call(
        "GET",
        format!("{}/api/pages/over?lang=nl", base(&handle)),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);
}
