//! Uploading a model, keeping it, sharing it and taking it — end to end against a
//! real listener.
//!
//! What a unit test cannot reach is the part that actually decides whether the
//! feature works: that a file uploaded by one account can be found and fetched by
//! a different one, that the licence a person stated is what the server refuses on
//! later, and that hiding a model takes it out of the *download* route and not
//! only out of the listing. The last of those is the one a half-implementation
//! gets wrong — a model taken out of the catalogue while still being served to
//! every prim already wearing it looks like moderation and is not.

use std::sync::Arc;

use cartopolis_simulator::{ensure_account, ensure_admin_account, serve, SimConfig, SimHandle};

/// A server with the HTTP listener up, an admin and two players.
async fn server() -> (SimHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_admin_account(&conn, "root", "adminpass").unwrap();
        ensure_account(&conn, "alice", "alicepw").unwrap();
        ensure_account(&conn, "bob", "bobpw").unwrap();
    }
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        http_listen: Some("127.0.0.1:0".into()),
        // Beside the database, so the temporary directory takes the files with it.
        asset_dir: dir.path().join("assets").to_str().unwrap().to_string(),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    (handle, dir)
}

fn base(handle: &SimHandle) -> String {
    format!("http://{}", handle.http_addr.expect("http listener bound"))
}

/// A JSON call. `ureq` is blocking, so it goes through `spawn_blocking` rather
/// than parking a runtime worker on a socket read — the same shape
/// `http_surface.rs` uses.
async fn api(
    method: &'static str,
    url: String,
    token: Option<String>,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let (status, bytes) = raw(
        method,
        url,
        token,
        body.map(|j| (j.to_string().into_bytes(), "application/json")),
    )
    .await;
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// A call whose body and response are bytes — which an upload and a download both
/// are, and which the JSON helper cannot express.
async fn raw(
    method: &'static str,
    url: String,
    token: Option<String>,
    body: Option<(Vec<u8>, &'static str)>,
) -> (u16, Vec<u8>) {
    tokio::task::spawn_blocking(move || {
        let mut req = ureq::request(method, &url);
        if let Some(token) = token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        let result = match body {
            Some((bytes, content_type)) => req.set("Content-Type", content_type).send_bytes(&bytes),
            None => req.call(),
        };
        let response = match result {
            Ok(r) => r,
            // A refusal is what several of these assertions are about, so it is a
            // result rather than a failure.
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => panic!("transport error: {e}"),
        };
        let status = response.status();
        let mut out = Vec::new();
        use std::io::Read as _;
        response.into_reader().read_to_end(&mut out).unwrap();
        (status, out)
    })
    .await
    .unwrap()
}

async fn sign_in(handle: &SimHandle, username: &str, password: &str) -> String {
    let (status, body) = api(
        "POST",
        format!("{}/api/login", base(handle)),
        None,
        Some(serde_json::json!({"username": username, "password": password})),
    )
    .await;
    assert_eq!(status, 200, "sign-in failed: {body}");
    body["token"].as_str().expect("a token").to_string()
}

async fn admin_sign_in(handle: &SimHandle) -> String {
    let (status, body) = api(
        "POST",
        format!("{}/api/admin/login", base(handle)),
        None,
        Some(serde_json::json!({"username": "root", "password": "adminpass"})),
    )
    .await;
    assert_eq!(status, 200, "admin sign-in failed: {body}");
    body["token"].as_str().expect("a token").to_string()
}

/// The smallest thing that is a model: one triangle, no textures, everything
/// inside the file. Written here rather than checked in, so the test says what it
/// is testing against instead of pointing at a binary.
fn one_triangle(scale: f32) -> Vec<u8> {
    let positions: [f32; 9] = [0.0, 0.0, 0.0, scale, 0.0, 0.0, 0.0, scale, 0.0];
    let mut bin = Vec::new();
    for v in positions {
        bin.extend_from_slice(&v.to_le_bytes());
    }
    let json = format!(
        r#"{{
            "asset": {{"version": "2.0"}},
            "scene": 0,
            "scenes": [{{"nodes": [0]}}],
            "nodes": [{{"mesh": 0}}],
            "meshes": [{{"primitives": [{{"attributes": {{"POSITION": 0}}}}]}}],
            "accessors": [{{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                           "min": [0,0,0], "max": [{scale},{scale},0]}}],
            "bufferViews": [{{"buffer": 0, "byteOffset": 0, "byteLength": 36}}],
            "buffers": [{{"byteLength": 36}}]
        }}"#
    );
    let mut json_chunk = json.into_bytes();
    while json_chunk.len() % 4 != 0 {
        json_chunk.push(b' ');
    }
    let total = 12 + 8 + json_chunk.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_chunk.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_chunk);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

async fn upload(
    handle: &SimHandle,
    token: &str,
    bytes: Vec<u8>,
    name: &str,
    licence: &str,
) -> (u16, serde_json::Value) {
    let url = format!("{}/api/assets?name={name}&licence={licence}", base(handle));
    let (status, body) = raw(
        "POST",
        url,
        Some(token.to_string()),
        Some((bytes, "model/gltf-binary")),
    )
    .await;
    (
        status,
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
    )
}

/// The whole path a person walks: upload, keep, share, and somebody else finds it
/// and fetches the same bytes back.
#[tokio::test]
async fn a_model_uploaded_by_one_account_reaches_another() {
    let (handle, _dir) = server().await;
    let alice = sign_in(&handle, "alice", "alicepw").await;
    let bob = sign_in(&handle, "bob", "bobpw").await;

    let model = one_triangle(1.0);
    let (status, item) = upload(&handle, &alice, model.clone(), "Bench", "cc0").await;
    assert_eq!(status, 201, "upload refused: {item}");
    let item_id = item["id"].as_i64().expect("an inventory id");
    let hash = item["asset"].as_str().expect("an asset hash").to_string();
    assert_eq!(item["triangles"].as_u64(), Some(1));

    // Alice holds it; Bob does not.
    let (_, mine) = api(
        "GET",
        format!("{}/api/inventory", base(&handle)),
        Some(alice.clone()),
        None,
    )
    .await;
    assert_eq!(mine.as_array().map(Vec::len), Some(1));
    let (_, bobs) = api(
        "GET",
        format!("{}/api/inventory", base(&handle)),
        Some(bob.clone()),
        None,
    )
    .await;
    assert_eq!(bobs.as_array().map(Vec::len), Some(0));

    // Sharing puts it in the catalogue, where Bob can see it.
    let (status, entry) = api(
        "POST",
        format!("{}/api/inventory/{item_id}/share", base(&handle)),
        Some(alice.clone()),
        Some(serde_json::json!({"category": "furniture"})),
    )
    .await;
    assert_eq!(status, 200, "share refused: {entry}");
    let (_, library) = api(
        "GET",
        format!("{}/api/library", base(&handle)),
        Some(bob.clone()),
        None,
    )
    .await;
    let entries = library.as_array().expect("a listing");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["asset"].as_str(), Some(hash.as_str()));
    assert_eq!(entries[0]["category"].as_str(), Some("furniture"));

    // And he can take a copy and fetch the file — which is the only definition of
    // "shared" that means anything.
    let (status, _) = api(
        "POST",
        format!("{}/api/library/{hash}/take", base(&handle)),
        Some(bob.clone()),
        None,
    )
    .await;
    assert_eq!(status, 201);
    let (status, bytes) = raw(
        "GET",
        format!("{}/api/assets/{hash}.glb", base(&handle)),
        Some(bob.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(bytes, model, "the bytes fetched are the bytes uploaded");
}

/// The file is named after its content, so two uploads of the same model are one
/// file — and two inventory items, because each person named their own.
#[tokio::test]
async fn the_same_file_twice_is_one_file_and_two_items() {
    let (handle, _dir) = server().await;
    let alice = sign_in(&handle, "alice", "alicepw").await;
    let bob = sign_in(&handle, "bob", "bobpw").await;
    let model = one_triangle(1.0);

    let (_, a) = upload(&handle, &alice, model.clone(), "Bench", "cc0").await;
    let (_, b) = upload(&handle, &bob, model, "Seat", "cc0").await;
    assert_eq!(a["asset"], b["asset"], "one file");
    assert_ne!(a["id"], b["id"], "two items");
    assert_eq!(a["name"].as_str(), Some("Bench"));
    assert_eq!(b["name"].as_str(), Some("Seat"));

    let admin = admin_sign_in(&handle).await;
    let (_, assets) = api(
        "GET",
        format!("{}/api/admin/assets", base(&handle)),
        Some(admin),
        None,
    )
    .await;
    let rows = assets.as_array().expect("a listing");
    assert_eq!(rows.len(), 1, "one row for one file");
    assert_eq!(rows[0]["held_by"].as_i64(), Some(2));
    assert_eq!(rows[0]["in_world"].as_i64(), Some(0));
}

/// The licence stated at upload is what the server refuses on. Using a model and
/// handing copies of it out are different acts under most asset licences, so
/// "something else" is usable and never shareable.
#[tokio::test]
async fn a_model_from_somewhere_else_cannot_be_shared() {
    let (handle, _dir) = server().await;
    let alice = sign_in(&handle, "alice", "alicepw").await;

    let (status, item) = upload(&handle, &alice, one_triangle(1.0), "Mystery", "other").await;
    assert_eq!(status, 201, "it may still be uploaded and used: {item}");
    let item_id = item["id"].as_i64().unwrap();

    let (status, body) = api(
        "POST",
        format!("{}/api/inventory/{item_id}/share", base(&handle)),
        Some(alice.clone()),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, 403);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("somewhere else"),
        "the refusal has to say why: {body}"
    );

    // And giving it away is the same act, so it earns the same refusal.
    let (status, _) = api(
        "POST",
        format!("{}/api/inventory/{item_id}/give", base(&handle)),
        Some(alice),
        Some(serde_json::json!({"to": "bob"})),
    )
    .await;
    assert_eq!(status, 403);
}

/// A CC-BY model needs the credit line at upload — the moment the person still
/// has the answer in front of them.
#[tokio::test]
async fn cc_by_needs_its_credit_line() {
    let (handle, _dir) = server().await;
    let alice = sign_in(&handle, "alice", "alicepw").await;

    let (status, body) = upload(&handle, &alice, one_triangle(1.0), "Tree", "cc-by").await;
    assert_eq!(status, 400, "{body}");
    assert!(body["error"].as_str().unwrap_or("").contains("credit"));

    let url = format!(
        "{}/api/assets?name=Tree&licence=cc-by&attribution=Tree%20by%20A.%20N.%20Other",
        base(&handle)
    );
    let (status, _) = raw(
        "POST",
        url,
        Some(alice),
        Some((one_triangle(1.0), "model/gltf-binary")),
    )
    .await;
    assert_eq!(status, 201);
}

/// Hiding a model stops it being *served*, not only listed. Prims already wearing
/// it fall back to the placeholder, which is what a report was about.
#[tokio::test]
async fn hiding_a_model_stops_the_download_too() {
    let (handle, _dir) = server().await;
    let alice = sign_in(&handle, "alice", "alicepw").await;
    let admin = admin_sign_in(&handle).await;

    let (_, item) = upload(&handle, &alice, one_triangle(1.0), "Rude", "own").await;
    let item_id = item["id"].as_i64().unwrap();
    let hash = item["asset"].as_str().unwrap().to_string();
    api(
        "POST",
        format!("{}/api/inventory/{item_id}/share", base(&handle)),
        Some(alice.clone()),
        Some(serde_json::json!({})),
    )
    .await;

    let (status, _) = raw(
        "GET",
        format!("{}/api/assets/{hash}.glb", base(&handle)),
        Some(alice.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200, "served before it is hidden");

    let (status, body) = api(
        "PATCH",
        format!("{}/api/admin/library/{hash}", base(&handle)),
        Some(admin),
        Some(serde_json::json!({"visibility": "hidden"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, _) = raw(
        "GET",
        format!("{}/api/assets/{hash}.glb", base(&handle)),
        Some(alice.clone()),
        None,
    )
    .await;
    assert_eq!(status, 410, "a hidden model is not served to anybody");

    let (_, library) = api(
        "GET",
        format!("{}/api/library", base(&handle)),
        Some(alice),
        None,
    )
    .await;
    assert_eq!(library.as_array().map(Vec::len), Some(0));
}

/// Not signed in is not "empty", it is refused — the store is per-server and
/// per-account, and an anonymous listing would be a way to read one without one.
#[tokio::test]
async fn the_store_needs_a_session() {
    let (handle, _dir) = server().await;
    let (status, _) = api(
        "GET",
        format!("{}/api/inventory", base(&handle)),
        None,
        None,
    )
    .await;
    assert_eq!(status, 401);
    let (status, _) = api("GET", format!("{}/api/library", base(&handle)), None, None).await;
    assert_eq!(status, 401);
}

/// A file that is not a self-contained binary glTF is refused, and the refusal
/// says what to do about it.
#[tokio::test]
async fn only_a_self_contained_glb_is_accepted() {
    let (handle, _dir) = server().await;
    let alice = sign_in(&handle, "alice", "alicepw").await;

    let (status, body) = upload(
        &handle,
        &alice,
        br#"{"asset":{"version":"2.0"}}"#.to_vec(),
        "Json",
        "own",
    )
    .await;
    assert_eq!(status, 400);
    assert!(
        body["error"].as_str().unwrap_or("").contains(".glb"),
        "{body}"
    );
}

/// A file nothing refers to may be forgotten; one a person still holds may not,
/// and the refusal says what is holding it.
#[tokio::test]
async fn a_file_in_use_cannot_be_forgotten() {
    let (handle, _dir) = server().await;
    let alice = sign_in(&handle, "alice", "alicepw").await;
    let admin = admin_sign_in(&handle).await;

    let (_, item) = upload(&handle, &alice, one_triangle(1.0), "Bench", "own").await;
    let item_id = item["id"].as_i64().unwrap();
    let hash = item["asset"].as_str().unwrap().to_string();

    let (status, body) = api(
        "DELETE",
        format!("{}/api/admin/assets/{hash}", base(&handle)),
        Some(admin.clone()),
        None,
    )
    .await;
    assert_eq!(status, 409, "{body}");

    // Once nobody holds it, it goes.
    let (status, _) = api(
        "DELETE",
        format!("{}/api/inventory/{item_id}", base(&handle)),
        Some(alice),
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (status, _) = api(
        "DELETE",
        format!("{}/api/admin/assets/{hash}", base(&handle)),
        Some(admin),
        None,
    )
    .await;
    assert_eq!(status, 204);
}
