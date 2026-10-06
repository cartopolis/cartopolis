//! The authoring API and everything it exists to reach, end to end.
//!
//! The unit tests cover the pieces — that a `ParticleEffect` sanitises, that the
//! script catalogue matches the installed API, that a prim row round-trips. What
//! they cannot see is the thing that was actually built: an operator with a token
//! and no viewer creating a prim, giving it a script, and that script flying it
//! across the map and reacting to somebody standing nearby.
//!
//! Three seams in particular were wired by hand and have nothing else watching
//! them:
//!
//! * **A prim authored over HTTP appears in the world**, not merely in SQLite —
//!   the world is the authority and holds prims in memory, so a row written
//!   behind its back would be invisible until a restart.
//! * **`set_geo` is durable horizontal motion.** `set_pos`'s x and z are a
//!   client-frame echo (ADR-029), so this was the one thing a script could not do
//!   before protocol 20, and "the prim moved" has to mean the *anchor* moved.
//! * **`sensor` sees an avatar**, which needs the avatar's `ClientIntent` to
//!   carry a geographic position, the sim to keep it, and `script_views` to
//!   measure it against the prim's anchor — three files, one answer.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, GeoAnchor, NetMessage, PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, ensure_admin_account, serve, SimConfig, SimHandle};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Client = Framed<TcpStream, LengthDelimitedCodec>;

/// Groningen's Grote Markt — where this world's content is anchored.
const LAT: f64 = 53.2194;
const LNG: f64 = 6.5665;

async fn server() -> (SimHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("authoring.db")
        .to_str()
        .unwrap()
        .to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_admin_account(&conn, "root", "adminpass").unwrap();
        ensure_account(&conn, "walker", "walkerpw").unwrap();
        ensure_account(&conn, "meddler", "meddlerpw").unwrap();
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
    format!("http://{}", handle.http_addr.expect("http listener bound"))
}

/// One admin API call. Blocking client on a blocking thread, as
/// `tests/http_surface.rs` does and for the same reason.
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

async fn token(handle: &SimHandle) -> String {
    let (status, body) = api(
        "POST",
        format!("{}/api/admin/login", base(handle)),
        None,
        Some(serde_json::json!({"username": "root", "password": "adminpass"})),
    )
    .await;
    assert_eq!(status, 200, "admin login: {body}");
    body["token"].as_str().unwrap().to_string()
}

async fn connect(addr: &str, username: &str, password: &str) -> Client {
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
            username: username.into(),
            password: password.into(),
        },
    })
    .unwrap();
    c.send(Bytes::from(hello)).await.unwrap();
    let ack = decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap();
    assert!(matches!(ack, NetMessage::ServerHelloAck { .. }), "{ack:?}");
    c
}

/// An intent that stands still and says where on Earth it is standing.
///
/// The whole reason protocol 20 added the field: an avatar's *sim* position is
/// metres in its own client's anchored frame, so the server cannot derive this
/// and a sensor has nothing to measure against a prim's absolute anchor.
async fn stand_at(c: &mut Client, lat: f64, lng: f64) {
    let msg = NetMessage::ClientIntent {
        request_id: 0,
        move_x: 0.0,
        move_z: 0.0,
        display_yaw: 0.0,
        flying: false,
        fly_up: false,
        fly_down: false,
        jump: false,
        emote: None,
        seated: None,
        geo: Some(GeoAnchor {
            latitude: lat,
            longitude: lng,
        }),
    };
    c.send(Bytes::from(encode_app_frame(&msg).unwrap()))
        .await
        .unwrap();
}

/// A session token for an ordinary (non-admin) account.
///
/// The same `/api/admin/login` route, which is not a contradiction: it mints from
/// the one session store the game uses, and it refuses a non-admin. So a player's
/// token has to come off the wire handshake instead — which is also the honest
/// test, because that is where a player's token comes from in real life.
async fn session(handle: &SimHandle, username: &str, password: &str) -> String {
    let stream = TcpStream::connect(handle.local_addr.to_string())
        .await
        .unwrap();
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
            username: username.into(),
            password: password.into(),
        },
    })
    .unwrap();
    c.send(Bytes::from(hello)).await.unwrap();
    match decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap() {
        NetMessage::ServerHelloAck { session_token, .. } => session_token,
        other => panic!("expected an ack, got {other:?}"),
    }
}

/// Build a prim over the wire, as a player does — the path that records an owner.
async fn build(c: &mut Client, name: &str, lat: f64, lng: f64) -> i64 {
    let msg = NetMessage::CreatePrim {
        request_id: 1,
        name: name.into(),
        position: glam::Vec3::new(0.0, 2.0, 0.0),
        rotation: glam::Vec3::ZERO,
        scale: glam::Vec3::ONE,
        color: [0.5, 0.5, 0.5],
        texture_id: None,
        shape: "box".into(),
        surface: cartopolis_core::PrimSurface::default(),
        geometry: cartopolis_core::PrimGeometry::default(),
        geo: GeoAnchor {
            latitude: lat,
            longitude: lng,
        },
        particles: None,
        model: None,
    };
    c.send(Bytes::from(encode_app_frame(&msg).unwrap()))
        .await
        .unwrap();
    recv_until(c, "prim upsert", |m| match m {
        NetMessage::PrimUpsert { prim } if prim.name == name => Some(prim.id),
        _ => None,
    })
    .await
}

/// Store a script without waiting for the outcome, so the caller can inspect it.
async fn send_script(c: &mut Client, prim_id: i64, source: &str) {
    let msg = NetMessage::SetPrimScript {
        request_id: 2,
        prim_id,
        source: source.into(),
    };
    c.send(Bytes::from(encode_app_frame(&msg).unwrap()))
        .await
        .unwrap();
}

async fn recv_output(c: &mut Client) -> (cartopolis_core::ScriptLevel, String) {
    recv_until(c, "script output", |m| match m {
        NetMessage::ScriptOutput { level, text, .. } => Some((level, text)),
        _ => None,
    })
    .await
}

async fn set_script(c: &mut Client, prim_id: i64, source: &str) {
    let msg = NetMessage::SetPrimScript {
        request_id: 2,
        prim_id,
        source: source.into(),
    };
    c.send(Bytes::from(encode_app_frame(&msg).unwrap()))
        .await
        .unwrap();
    recv_until(c, "the script to compile", |m| match m {
        NetMessage::ScriptOutput { .. } => Some(()),
        _ => None,
    })
    .await;
}

/// Read frames until one matches, discarding the snapshot traffic between.
async fn recv_until<T>(
    c: &mut Client,
    what: &str,
    mut f: impl FnMut(NetMessage) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let frame = tokio::time::timeout_at(deadline, c.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .unwrap()
            .unwrap();
        if let Some(v) = f(decode_app_frame(&frame).unwrap()) {
            return v;
        }
    }
}

/// Press a prim, the way walking up to it and pressing E does.
async fn press(c: &mut Client, prim_id: i64) {
    let msg = NetMessage::TouchPrim {
        request_id: 0,
        prim_id,
    };
    c.send(Bytes::from(encode_app_frame(&msg).unwrap()))
        .await
        .unwrap();
}

/// Poll the prim over the admin API until `done` says yes, or give up.
///
/// Reading it back through the API rather than through the wire on purpose: the
/// authoring surface is what an author sees, and if a script's writes did not
/// reach it then the prim moved for the renderer and nowhere else.
async fn wait_for(
    handle: &SimHandle,
    tok: &str,
    prim_id: i64,
    what: &str,
    mut done: impl FnMut(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let (status, body) = api(
            "GET",
            format!("{}/api/admin/prims/{prim_id}", base(handle)),
            Some(tok.to_string()),
            None,
        )
        .await;
        if status == 200 && done(&body) {
            return body;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for {what}; last answer was {status}: {body}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The plain authoring loop, with no script in it: make a prim, change it, take
/// its emitter off, delete it.
#[tokio::test]
async fn an_operator_can_build_without_a_viewer() {
    let (handle, _dir) = server().await;
    let tok = token(&handle).await;

    let (status, created) = api(
        "POST",
        format!("{}/api/admin/prims", base(&handle)),
        Some(tok.clone()),
        Some(serde_json::json!({
            "name": "Bonfire",
            "shape": "cylinder",
            "latitude": LAT,
            "longitude": LNG,
            "altitude": 2.0,
            // Degrees at this boundary, radians on the wire.
            "rotation": [0.0, 90.0, 0.0],
            "particles": { "count": 32, "life": 1.0, "burst": false, "sequence": 1 },
        })),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let prim_id = created["id"].as_i64().unwrap();
    assert_eq!(created["name"], "Bonfire");
    assert_eq!(created["particles"]["count"], 32);
    assert_eq!(created["scripted"], false);
    let yaw = created["rotation"][1].as_f64().unwrap();
    assert!(
        (yaw - std::f64::consts::FRAC_PI_2).abs() < 1e-4,
        "90 degrees should have reached the wire as radians, got {yaw}"
    );

    // A patch names one field and leaves the rest — including the emitter, which
    // is the field most easily wiped by a "send the whole prim" update.
    let (status, patched) = api(
        "PATCH",
        format!("{}/api/admin/prims/{prim_id}", base(&handle)),
        Some(tok.clone()),
        Some(serde_json::json!({ "color": [1.0, 0.4, 0.1] })),
    )
    .await;
    assert_eq!(status, 200, "{patched}");
    assert_eq!(patched["name"], "Bonfire", "a patch is not a replace");
    assert_eq!(patched["particles"]["count"], 32, "the emitter survived");

    // An explicit null is how you take one off, and it has to be distinguishable
    // from the absent field above.
    let (status, doused) = api(
        "PATCH",
        format!("{}/api/admin/prims/{prim_id}", base(&handle)),
        Some(tok.clone()),
        Some(serde_json::json!({ "particles": null })),
    )
    .await;
    assert_eq!(status, 200, "{doused}");
    assert!(doused["particles"].is_null(), "{doused}");

    let (status, _) = api(
        "DELETE",
        format!("{}/api/admin/prims/{prim_id}", base(&handle)),
        Some(tok.clone()),
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (status, body) = api(
        "GET",
        format!("{}/api/admin/prims/{prim_id}", base(&handle)),
        Some(tok),
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");
}

/// An emitter is described the way a script describes one: name what differs,
/// and the rest comes from a preset.
///
/// The alternative — and what this API did for an afternoon — is that authoring a
/// fire means writing fourteen fields, and getting one wrong is a 422 listing the
/// other thirteen. It also has to match `particles`, or the same effect is
/// spelled two ways depending on which surface set it.
#[tokio::test]
async fn an_emitter_can_be_described_by_what_differs_from_the_preset() {
    let (handle, _dir) = server().await;
    let tok = token(&handle).await;

    let (status, blue) = api(
        "POST",
        format!("{}/api/admin/prims", base(&handle)),
        Some(tok.clone()),
        Some(serde_json::json!({
            "latitude": LAT,
            "longitude": LNG,
            "particles": { "color_start": [0.2, 0.6, 4.0] },
        })),
    )
    .await;
    assert_eq!(status, 201, "{blue}");
    let prim_id = blue["id"].as_i64().unwrap();
    assert_eq!(blue["particles"]["color_start"][2], 4.0);
    assert_eq!(
        blue["particles"]["life"],
        cartopolis_core::ParticleEffect::fire().life,
        "the unmentioned fields should be the fire preset's: {blue}"
    );
    assert_eq!(blue["particles"]["burst"], false);

    // A blast takes the *other* preset's defaults, which is the same choice
    // `particle_burst` makes against `particles`.
    let (status, blast) = api(
        "PATCH",
        format!("{}/api/admin/prims/{prim_id}", base(&handle)),
        Some(tok.clone()),
        Some(serde_json::json!({ "particles": { "burst": true } })),
    )
    .await;
    assert_eq!(status, 200, "{blast}");
    assert_eq!(
        blast["particles"]["spread_deg"], 180.0,
        "a blast goes in every direction: {blast}"
    );
    let first = blast["particles"]["sequence"].as_u64().unwrap();

    // Authoring the same blast again is a *second* explosion, and the sequence is
    // the only thing that says so — a client replays a burst once per
    // `(prim, sequence)`, so an unchanged number is a bang that never happens.
    let (status, again) = api(
        "PATCH",
        format!("{}/api/admin/prims/{prim_id}", base(&handle)),
        Some(tok),
        Some(serde_json::json!({ "particles": { "burst": true } })),
    )
    .await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(
        again["particles"]["sequence"].as_u64().unwrap(),
        first + 1,
        "re-authoring a burst should re-arm it: {again}"
    );
}

/// A player exports their own build and imports it back.
///
/// The round trip is the test worth having: an export whose document the import
/// route will not accept is not an export, and the two halves are only guaranteed
/// to agree because they are the *same* `PrimSpec` the create route takes.
#[tokio::test]
async fn a_player_can_export_their_own_build_and_import_it_back() {
    let (handle, _dir) = server().await;
    let admin = token(&handle).await;
    let walker = session(&handle, "walker", "walkerpw").await;

    // Built over the wire, which is the path that records an owner.
    let mut c = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    let prim_id = build(&mut c, "Lamp", LAT, LNG).await;
    set_script(&mut c, prim_id, "function on_touch(id, who) say(who) end").await;

    // Give it something worth carrying, so the document is not trivially equal.
    let (status, _) = api(
        "PATCH",
        format!("{}/api/admin/prims/{prim_id}", base(&handle)),
        Some(admin.clone()),
        Some(serde_json::json!({
            "color": [0.2, 0.6, 0.9],
            "rotation": [0.0, 90.0, 0.0],
            "particles": { "color_start": [0.2, 0.6, 4.0] },
        })),
    )
    .await;
    assert_eq!(status, 200);

    let (status, doc) = api(
        "GET",
        format!("{}/api/content/export", base(&handle)),
        Some(walker.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200, "{doc}");
    assert_eq!(doc["version"], 1);
    let prims = doc["prims"].as_array().unwrap();
    assert_eq!(prims.len(), 1, "one owned prim: {doc}");
    let exported = &prims[0];
    assert_eq!(exported["name"], "Lamp");
    assert_eq!(exported["color"], serde_json::json!([0.2, 0.6, 0.9]));
    // Degrees on this surface, radians on the wire — an export that wrote radians
    // would re-import as a prim turned by 5157 degrees.
    assert!(
        (exported["rotation"][1].as_f64().unwrap() - 90.0).abs() < 1e-3,
        "rotation should come back in degrees: {exported}"
    );
    assert_eq!(exported["particles"]["color_start"][2], 4.0);
    assert!(
        exported["script"].as_str().unwrap().contains("on_touch"),
        "the script is the part an export exists to carry: {exported}"
    );
    // Server-assigned facts are deliberately absent: an id from one server means
    // nothing on another.
    assert!(
        exported.get("region_id").is_none(),
        "a region is not a thing a document can name"
    );

    // …and it goes straight back in.
    let (status, back) = api(
        "POST",
        format!("{}/api/content/import", base(&handle)),
        Some(walker.clone()),
        Some(doc.clone()),
    )
    .await;
    assert_eq!(status, 200, "{back}");
    assert_eq!(back["imported"].as_array().unwrap().len(), 1, "{back}");

    // Import *adds*: there are two now, and the second is owned by the importer,
    // so a second export sees both.
    let (_, doc2) = api(
        "GET",
        format!("{}/api/content/export", base(&handle)),
        Some(walker),
        None,
    )
    .await;
    assert_eq!(
        doc2["prims"].as_array().unwrap().len(),
        2,
        "import adds rather than replaces: {doc2}"
    );
}

/// A player's *own* export carries only their own prims — including the admin's,
/// whose default scope is no wider than anybody else's.
///
/// The default is what matters here. An account cannot read somebody else's build
/// out of this route by any argument it supplies; reaching further is an explicit
/// query and an explicit permission (the next test).
#[tokio::test]
async fn an_export_defaults_to_the_callers_own_prims() {
    let (handle, _dir) = server().await;
    let admin = token(&handle).await;

    // One prim built by `walker` over the wire, two installed by the server.
    let mut c = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    build(&mut c, "Walkers lamp", LAT, LNG).await;
    let (status, _) = api(
        "POST",
        format!("{}/api/admin/content/firebird", base(&handle)),
        Some(admin.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200);

    // The admin has built nothing, so their *own* export is empty — the content
    // pack's prims belong to the server, not to whoever installed them.
    let (status, doc) = api(
        "GET",
        format!("{}/api/content/export", base(&handle)),
        Some(admin.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200, "{doc}");
    assert!(
        doc["prims"].as_array().unwrap().is_empty(),
        "an admin's own export is their own build, which is nothing: {doc}"
    );

    // Walker gets exactly their own — not the two the content pack installed.
    let walker = session(&handle, "walker", "walkerpw").await;
    let (_, mine) = api(
        "GET",
        format!("{}/api/content/export", base(&handle)),
        Some(walker.clone()),
        None,
    )
    .await;
    let names: Vec<&str> = mine["prims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Walkers lamp"], "{mine}");

    // And a player cannot reach past themselves by asking.
    for query in ["?all=true", "?owner=walker"] {
        let (status, body) = api(
            "GET",
            format!("{}/api/content/export{query}", base(&handle)),
            Some(walker.clone()),
            None,
        )
        .await;
        assert_eq!(
            status, 403,
            "a player must not reach past themselves: {body}"
        );
    }
}

/// An admin can always export — their own, a named account's, or the whole world.
#[tokio::test]
async fn an_admin_can_export_anything() {
    let (handle, _dir) = server().await;
    let admin = token(&handle).await;

    let mut c = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    build(&mut c, "Walkers lamp", LAT, LNG).await;
    let (status, _) = api(
        "POST",
        format!("{}/api/admin/content/firebird", base(&handle)),
        Some(admin.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200);

    // By owner.
    let (status, theirs) = api(
        "GET",
        format!("{}/api/content/export?owner=walker", base(&handle)),
        Some(admin.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200, "{theirs}");
    assert_eq!(theirs["prims"].as_array().unwrap().len(), 1, "{theirs}");
    assert_eq!(theirs["prims"][0]["name"], "Walkers lamp");

    // Everything, which is the only way to reach the server's own prims.
    let (status, all) = api(
        "GET",
        format!("{}/api/content/export?all=true", base(&handle)),
        Some(admin.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200, "{all}");
    let names: Vec<&str> = all["prims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"Walkers lamp") && names.iter().any(|n| n.starts_with("Firebird")),
        "a whole-world export includes unowned prims: {names:?}"
    );

    // A username nobody has is a 404, not an empty document that reads as
    // "they have built nothing".
    let (status, body) = api(
        "GET",
        format!("{}/api/content/export?owner=nobody", base(&handle)),
        Some(admin),
        None,
    )
    .await;
    assert_eq!(status, 404, "{body}");
}

/// Ownership can be handed over — by the owner, and by an admin.
#[tokio::test]
async fn ownership_can_be_transferred() {
    let (handle, _dir) = server().await;
    let admin = token(&handle).await;
    let walker = session(&handle, "walker", "walkerpw").await;

    let mut c = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    let mine = build(&mut c, "Walkers lamp", LAT, LNG).await;

    // The owner gives it to root.
    let (status, body) = api(
        "POST",
        format!("{}/api/content/transfer", base(&handle)),
        Some(walker.clone()),
        Some(serde_json::json!({ "prims": [mine], "to": "root" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    // It is root's now, and walker's export is empty.
    let (_, roots) = api(
        "GET",
        format!("{}/api/content/export", base(&handle)),
        Some(admin.clone()),
        None,
    )
    .await;
    assert_eq!(roots["prims"].as_array().unwrap().len(), 1, "{roots}");
    let (_, walkers) = api(
        "GET",
        format!("{}/api/content/export", base(&handle)),
        Some(walker.clone()),
        None,
    )
    .await;
    assert!(walkers["prims"].as_array().unwrap().is_empty(), "{walkers}");

    // Walker cannot take it back — it is not theirs to give any more.
    let (status, body) = api(
        "POST",
        format!("{}/api/content/transfer", base(&handle)),
        Some(walker),
        Some(serde_json::json!({ "prims": [mine], "to": "walker" })),
    )
    .await;
    assert_eq!(status, 403, "{body}");

    // The admin can, because somebody has to be able to re-home a build.
    let (status, body) = api(
        "POST",
        format!("{}/api/content/transfer", base(&handle)),
        Some(admin),
        Some(serde_json::json!({ "prims": [mine], "to": "walker" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

/// A transfer is all-or-nothing: one prim you may not move refuses the request
/// and leaves the rest where they were.
///
/// A partial transfer is the worst outcome available, because the response gives
/// the caller no way to tell which half happened.
#[tokio::test]
async fn a_transfer_moves_all_of_them_or_none() {
    let (handle, _dir) = server().await;
    let admin = token(&handle).await;
    let walker = session(&handle, "walker", "walkerpw").await;

    let mut c = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    let mine = build(&mut c, "Mine", LAT, LNG).await;

    // The pack's prims belong to the server, so walker may move neither.
    let (status, installed) = api(
        "POST",
        format!("{}/api/admin/content/firebird", base(&handle)),
        Some(admin),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let theirs = installed["installed"][0]["id"].as_i64().unwrap();

    let (status, body) = api(
        "POST",
        format!("{}/api/content/transfer", base(&handle)),
        Some(walker.clone()),
        Some(serde_json::json!({ "prims": [mine, theirs], "to": "root" })),
    )
    .await;
    assert_eq!(status, 403, "{body}");

    // …and the one they *did* own has not moved.
    let (_, still) = api(
        "GET",
        format!("{}/api/content/export", base(&handle)),
        Some(walker),
        None,
    )
    .await;
    assert_eq!(
        still["prims"].as_array().unwrap().len(),
        1,
        "the legal half must not have gone through on its own: {still}"
    );
}

/// Signing in is the whole gate, and there is no anonymous export.
#[tokio::test]
async fn import_and_export_need_a_session() {
    let (handle, _dir) = server().await;
    for (method, path) in [("GET", "export"), ("POST", "import")] {
        let (status, _) = api(
            method,
            format!("{}/api/content/{path}", base(&handle)),
            None,
            Some(serde_json::json!({"version": 1, "prims": []})),
        )
        .await;
        assert_eq!(
            status, 401,
            "{method} /api/content/{path} must need a session"
        );
    }
}

/// A document from a newer server is refused rather than half-read.
#[tokio::test]
async fn a_document_from_the_future_is_refused() {
    let (handle, _dir) = server().await;
    let walker = session(&handle, "walker", "walkerpw").await;
    let (status, body) = api(
        "POST",
        format!("{}/api/content/import", base(&handle)),
        Some(walker),
        Some(serde_json::json!({
            "version": 99,
            "prims": [{"latitude": LAT, "longitude": LNG}],
        })),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body["error"].as_str().unwrap().contains("99"), "{body}");
}

/// Ask the server to rename a prim, and report whether it refused.
///
/// The name is the probe: it comes back in the `PrimUpsert` either way, so a test
/// can tell an accepted edit from a refused one *and* check what the server's own
/// copy says afterwards.
async fn try_rename(c: &mut Client, prim_id: i64, to: &str) -> Option<(u32, String)> {
    let msg = NetMessage::UpdatePrim {
        request_id: 77,
        prim_id,
        position: glam::Vec3::new(0.0, 9.0, 0.0),
        rotation: glam::Vec3::ZERO,
        scale: glam::Vec3::ONE,
        color: [1.0, 0.0, 0.0],
        texture_id: None,
        name: to.into(),
        surface: cartopolis_core::PrimSurface::default(),
        geometry: cartopolis_core::PrimGeometry::default(),
        geo: GeoAnchor {
            latitude: LAT,
            longitude: LNG,
        },
        particles: None,
        model: None,
    };
    c.send(Bytes::from(encode_app_frame(&msg).unwrap()))
        .await
        .unwrap();
    // Either a refusal comes back, or the change is broadcast. Both are answers;
    // waiting only for one would hang on the other.
    recv_until(c, "an answer to the edit", |m| match m {
        NetMessage::ServerError { code, message, .. } => Some(Some((code, message))),
        NetMessage::PrimUpsert { prim } if prim.id == prim_id && prim.name == to => Some(None),
        _ => None,
    })
    .await
}

/// **Editing is owner-scoped, and an admin overrides it.**
///
/// Before this, `UpdatePrim` and `DeletePrim` were open to any connected client:
/// anything anybody built could be moved, recoloured, rescripted or deleted by
/// anybody else. This is the test that says otherwise.
#[tokio::test]
async fn only_the_owner_or_an_admin_may_change_a_prim() {
    let (handle, _dir) = server().await;
    let addr = handle.local_addr.to_string();

    let mut walker = connect(&addr, "walker", "walkerpw").await;
    let theirs = build(&mut walker, "Walkers lamp", LAT, LNG).await;

    // The owner may change their own.
    assert!(
        try_rename(&mut walker, theirs, "Walkers own edit")
            .await
            .is_none(),
        "the owner must be able to edit what they built"
    );

    // A second player may not.
    let mut other = connect(&addr, "meddler", "meddlerpw").await;
    let refused = try_rename(&mut other, theirs, "Stolen").await;
    let (code, message) = refused.expect("a stranger's edit must be refused");
    assert_eq!(code, 403, "{message}");
    assert!(
        message.contains("somebody else"),
        "the refusal should say why: {message}"
    );

    // …and the truth follows it, so the refused client stops drawing the edit it
    // had already applied locally. Without this a rejected move is permanent on
    // the screen of whoever tried it.
    let corrected = recv_until(&mut other, "the authoritative prim", |m| match m {
        NetMessage::PrimUpsert { prim } if prim.id == theirs => Some(prim),
        _ => None,
    })
    .await;
    assert_eq!(
        corrected.name, "Walkers own edit",
        "the correction must carry the state the server actually holds, not the \
         one the refused client tried to write"
    );

    // …and cannot take it over by scripting it either, which would be the most
    // complete way around an edit check.
    send_script(&mut other, theirs, "function on_start() die() end").await;
    let (level, text) = recv_output(&mut other).await;
    assert_eq!(level, cartopolis_core::ScriptLevel::Error, "{text}");
    assert!(text.contains("somebody else"), "{text}");

    // An admin overrides both.
    let mut root = connect(&addr, "root", "adminpass").await;
    assert!(
        try_rename(&mut root, theirs, "Admin override")
            .await
            .is_none(),
        "an admin must be able to change anything"
    );
}

/// An unowned prim belongs to the server, so a player may not delete it.
///
/// This is the deliberate side of a real trade: treating "no owner" as
/// "anybody's" would leave the firebird and the beacon deletable by any
/// passer-by, and everything built before schema V15 is unowned.
#[tokio::test]
async fn a_player_cannot_delete_what_the_server_installed() {
    let (handle, _dir) = server().await;
    let admin = token(&handle).await;
    let (status, installed) = api(
        "POST",
        format!("{}/api/admin/content/firebird", base(&handle)),
        Some(admin),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let beacon = installed["installed"][1]["id"].as_i64().unwrap();

    let mut walker = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    let msg = NetMessage::DeletePrim {
        request_id: 88,
        prim_id: beacon,
    };
    walker
        .send(Bytes::from(encode_app_frame(&msg).unwrap()))
        .await
        .unwrap();
    let (code, message) = recv_until(&mut walker, "a refusal", |m| match m {
        NetMessage::ServerError { code, message, .. } => Some((code, message)),
        NetMessage::PrimRemoved { id } if id == beacon => {
            panic!("a player deleted a content pack's prim")
        }
        _ => None,
    })
    .await;
    assert_eq!(code, 403, "{message}");
}

/// The API refuses what the renderer would have accepted silently.
#[tokio::test]
async fn the_api_refuses_a_shape_the_client_would_have_drawn_as_a_box() {
    let (handle, _dir) = server().await;
    let tok = token(&handle).await;
    let (status, body) = api(
        "POST",
        format!("{}/api/admin/prims", base(&handle)),
        Some(tok),
        Some(serde_json::json!({
            "shape": "spehre",
            "latitude": LAT,
            "longitude": LNG,
        })),
    )
    .await;
    // `PrimShape::from_str` falls back to a box, so without this check the typo
    // would have been stored, drawn as a cube, and reported as a success.
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("spehre"),
        "the message should name what was wrong: {body}"
    );
}

/// Nobody without an admin token gets to build anything.
#[tokio::test]
async fn authoring_needs_an_admin() {
    let (handle, _dir) = server().await;
    let (status, _) = api(
        "GET",
        format!("{}/api/admin/prims", base(&handle)),
        None,
        None,
    )
    .await;
    assert_eq!(status, 401);

    // A real player's real token is not enough either: the wire is where a player
    // builds, and it is checked against the world's budget.
    let (status, body) = api(
        "POST",
        format!("{}/api/admin/login", base(&handle)),
        None,
        Some(serde_json::json!({"username": "walker", "password": "walkerpw"})),
    )
    .await;
    assert_eq!(
        status, 401,
        "a non-admin must not get an admin token: {body}"
    );
}

/// The button on the Grote Markt, end to end: install the pack, press the beacon,
/// watch the firebird come in, press it again and watch it go back out.
///
/// This is the one test that exercises **script-to-script messaging**, which is
/// the whole reason a button can exist at all: every host call acts on the
/// script's own prim, so a beacon cannot reach a firebird except over a channel.
/// It is written against the shipped content rather than a fixture on purpose —
/// the pair only works if they agree about a channel and about what a press
/// means, and a test with its own two scripts would keep passing while the
/// installed ones drifted apart.
#[tokio::test]
async fn the_beacon_summons_the_firebird_and_sends_it_back() {
    let (handle, _dir) = server().await;
    let tok = token(&handle).await;

    let (status, body) = api(
        "POST",
        format!("{}/api/admin/content/firebird", base(&handle)),
        Some(tok.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let installed = body["installed"].as_array().unwrap();
    assert_eq!(installed.len(), 2, "the pack is a pair: {body}");
    let beacon = installed
        .iter()
        .find(|p| p["name"] == "Beacon")
        .expect("a beacon")["id"]
        .as_i64()
        .unwrap();
    let firebird = installed
        .iter()
        .find(|p| p["name"] == "Firebird")
        .expect("a firebird")["id"]
        .as_i64()
        .unwrap();

    // The beacon asks on startup and the firebird answers, so the label is right
    // before anybody has pressed anything. That round trip is `send` out and
    // `on_message` back, and nothing else could have set this string.
    wait_for(&handle, &tok, beacon, "the beacon to hear back", |p| {
        p["name"].as_str().is_some_and(|n| n.contains("summon"))
    })
    .await;

    // A press is a touch, exactly as walking up to it and pressing E sends one.
    let mut player = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    press(&mut player, beacon).await;

    // It comes in: over the beacon rather than out on its 180 m circle.
    let here = wait_for(&handle, &tok, firebird, "the firebird to come in", |p| {
        let lat = p["geo"]["latitude"].as_f64().unwrap_or(0.0);
        let lng = p["geo"]["longitude"].as_f64().unwrap_or(0.0);
        cartopolis_core::ground_distance_m(LAT, LNG, lat, lng) < 20.0
    })
    .await;
    assert!(
        here["position"][1].as_f64().unwrap() < 20.0,
        "summoned means low enough to look at, not still at patrol height: {here}"
    );
    // And the beacon knows, because the firebird told it.
    wait_for(&handle, &tok, beacon, "the beacon to say it is here", |p| {
        p["name"].as_str().is_some_and(|n| n.contains("release"))
    })
    .await;

    // Pressing again sends it back out. The same message; the firebird decides
    // which way, because it is the only one that knows what it is doing.
    press(&mut player, beacon).await;
    wait_for(
        &handle,
        &tok,
        firebird,
        "the firebird to go back out",
        |p| {
            let lat = p["geo"]["latitude"].as_f64().unwrap_or(0.0);
            let lng = p["geo"]["longitude"].as_f64().unwrap_or(0.0);
            cartopolis_core::ground_distance_m(LAT, LNG, lat, lng) > 150.0
        },
    )
    .await;
    wait_for(&handle, &tok, beacon, "the beacon to say it is away", |p| {
        p["name"].as_str().is_some_and(|n| n.contains("summon"))
    })
    .await;
}

/// Installing a pack twice replaces what it installed, and matches on
/// **provenance** rather than on the prim names it declares.
///
/// The name version was the first one and the pack broke it itself: the beacon's
/// script renames it to whatever its label currently says, so the second install
/// found nothing called "Beacon", left the old one standing and added another.
/// Measured, not reasoned about — after two installs there were three prims.
#[tokio::test]
async fn reinstalling_a_pack_replaces_what_it_installed_even_after_a_rename() {
    let (handle, _dir) = server().await;
    let tok = token(&handle).await;
    let install = || {
        let tok = tok.clone();
        let url = format!("{}/api/admin/content/firebird", base(&handle));
        async move { api("POST", url, Some(tok), None).await }
    };

    let (status, first) = install().await;
    assert_eq!(status, 200, "{first}");

    // Let the beacon's `on_start` run, which is what renames it.
    wait_for(
        &handle,
        &tok,
        first["installed"][1]["id"].as_i64().unwrap(),
        "the beacon to rename itself",
        |p| p["name"].as_str().is_some_and(|n| n != "Beacon"),
    )
    .await;

    let (status, second) = install().await;
    assert_eq!(status, 200, "{second}");
    assert_eq!(
        second["replaced"].as_array().unwrap().len(),
        2,
        "both prims should have been replaced, renamed or not: {second}"
    );

    let (_, listed) = api(
        "GET",
        format!("{}/api/admin/prims", base(&handle)),
        Some(tok),
        None,
    )
    .await;
    assert_eq!(
        listed["prims"].as_array().unwrap().len(),
        2,
        "two installs should leave one pack, not one and a half: {listed}"
    );
}

/// The startle-and-explode path, in the shape the shipped firebird has it: a
/// scripted prim
/// flies a circle, notices an avatar, shakes, explodes and removes itself.
///
/// Deliberately one test rather than four. Each step is only meaningful because
/// the one before it happened — a sensor that fires on a prim that never moved
/// proves nothing about the sensor — and the sequence *is* the feature.
#[tokio::test]
async fn a_scripted_prim_circles_senses_and_explodes() {
    let (handle, _dir) = server().await;
    let tok = token(&handle).await;

    // Small numbers so the test is seconds rather than a minute: a 40 m circle, a
    // quick lap, and a shake short enough not to dominate the run. The shape is
    // the shipped firebird's exactly, except that this one ends on `die` —
    // the shipped one reforms, and `die` is the arm nothing else covers.
    let source = r#"
        local LAT, LNG = 53.2194, 6.5665
        local RADIUS, LAP, NOTICE, SHAKE = 40.0, 4.0, 60.0, 0.4
        local M_LAT, M_LNG = geo_scale(LAT)
        local state, marked = "flying", 0

        function on_start()
            set_name("Firebird")
            particles{ count = 64, life = 1.0 }
            set_timer_event(0.05)
        end

        function on_timer()
            local now = get_time()
            if state == "flying" then
                local a = (now / LAP) * 2 * math.pi
                set_geo(LAT + (RADIUS * math.cos(a)) / M_LAT,
                         LNG + (RADIUS * math.sin(a)) / M_LNG)
                local id, who = sensor(NOTICE)
                if id then
                    state, marked = "shaking", now
                    set_name("Firebird sees " .. who)
                end
            elseif state == "shaking" then
                local lat, lng = get_geo()
                set_geo(lat + 0.4 * math.sin(now * 47.0) / M_LAT, lng)
                if now - marked >= SHAKE then
                    state, marked = "blown", now
                    particle_burst{ count = 200, life = 0.5 }
                    set_alpha(0.0)
                end
            elseif state == "blown" and now - marked >= 0.3 then
                die()
            end
        end
    "#;

    let (status, created) = api(
        "POST",
        format!("{}/api/admin/prims", base(&handle)),
        Some(tok.clone()),
        Some(serde_json::json!({
            "name": "Firebird",
            "shape": "cylinder",
            "latitude": LAT,
            "longitude": LNG,
            "altitude": 20.0,
            "script": source,
        })),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let prim_id = created["id"].as_i64().unwrap();
    assert_eq!(
        created["scripted"], true,
        "the script was stored: {created}"
    );

    // ── It flies. ────────────────────────────────────────────────────────────
    // The *anchor* has to move, not `position.x/z` — those are a client-frame
    // echo that every client re-derives from the anchor, so a prim that "moved"
    // only in them has not moved at all.
    let moved = wait_for(&handle, &tok, prim_id, "the prim to fly", |p| {
        let lat = p["geo"]["latitude"].as_f64().unwrap_or(LAT);
        let lng = p["geo"]["longitude"].as_f64().unwrap_or(LNG);
        cartopolis_core::ground_distance_m(LAT, LNG, lat, lng) > 20.0
    })
    .await;
    let radius = cartopolis_core::ground_distance_m(
        LAT,
        LNG,
        moved["geo"]["latitude"].as_f64().unwrap(),
        moved["geo"]["longitude"].as_f64().unwrap(),
    );
    assert!(
        (30.0..50.0).contains(&radius),
        "it should be on its 40 m circle, not {radius:.1} m out"
    );
    assert_eq!(
        moved["particles"]["count"], 64,
        "the flame should be on the prim the API reports: {moved}"
    );

    // ── It notices somebody. ─────────────────────────────────────────────────
    // Nobody is in the world yet, so it is still flying — which is the control
    // for the assertion below: a sensor that fired without an avatar would be
    // measuring against something else.
    assert_eq!(moved["name"], "Firebird", "nobody is there yet: {moved}");

    let mut walker = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    // Standing at the centre of the circle: 40 m from the prim wherever it is on
    // its lap, comfortably inside the script's 60 m notice range.
    //
    // Repeated, because an intent is rate-limited and a single one could be
    // dropped — and because the *drain* below is what stops the client's socket
    // buffer filling with snapshots and back-pressuring the server's broadcast.
    let sensed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            stand_at(&mut walker, LAT, LNG).await;
            let _ = tokio::time::timeout(Duration::from_millis(50), walker.next()).await;
            let (status, body) = api(
                "GET",
                format!("{}/api/admin/prims/{prim_id}", base(&handle)),
                Some(tok.clone()),
                None,
            )
            .await;
            // Gone already? Then it sensed, shook and blew up between polls,
            // which is the whole sequence and is a pass for this step.
            if status == 404 {
                return None;
            }
            if body["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("Firebird sees"))
            {
                return Some(body);
            }
        }
    })
    .await
    .expect("the prim never noticed the avatar standing next to it");

    if let Some(body) = sensed {
        assert_eq!(
            body["name"], "Firebird sees walker",
            "the sensor should hand the script the account's name: {body}"
        );
    }

    // ── It shakes, explodes and goes. ────────────────────────────────────────
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        // Keep draining, or the connection's snapshot backlog stalls the tick
        // loop's broadcast and the script stops being stepped.
        let _ = tokio::time::timeout(Duration::from_millis(50), walker.next()).await;
        let (status, body) = api(
            "GET",
            format!("{}/api/admin/prims/{prim_id}", base(&handle)),
            Some(tok.clone()),
            None,
        )
        .await;
        if status == 404 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "die never removed the prim; it was last {body}"
        );
    }

    // And the world got its budget back — a prim that is gone from the world but
    // not from the counters is a world that fills up with nothing.
    let (_, capacity) = api(
        "GET",
        format!("{}/api/admin/capacity", base(&handle)),
        Some(tok),
        None,
    )
    .await;
    assert_eq!(
        capacity["prims_used"], 0,
        "the exploded prim should have released its budget: {capacity}"
    );
}

/// The football pack is a game: the ball is physical, an avatar's kick puts it
/// in the net, and the ball's own script calls the goal.
///
/// Physics, the kick, the goals as static colliders and the referee script are
/// four pieces that each pass on their own; this is the one place all four have
/// to agree — about where the goal line is, above all.
#[tokio::test]
async fn a_kick_into_the_north_goal_is_called_a_goal() {
    let (handle, _dir) = server().await;
    let tok = token(&handle).await;
    let (status, body) = api(
        "POST",
        format!("{}/api/admin/content/football", base(&handle)),
        Some(tok.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let ball = body["installed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Football")
        .expect("a football")["id"]
        .as_i64()
        .unwrap();

    // Dropped from a metre, it lands on the centre spot.
    wait_for(&handle, &tok, ball, "the ball to land", |p| {
        p["physical"] == true && p["position"][1].as_f64().is_some_and(|y| y < 0.2)
    })
    .await;

    // Put it three metres in front of the north goal.
    const NORTH_GOAL: (f64, f64) = (53.20661975, 6.5916658);
    let (m_lat, _) = cartopolis_core::degree_scale_m(NORTH_GOAL.0);
    let spot = (NORTH_GOAL.0 - 3.0 / m_lat, NORTH_GOAL.1);
    let (status, body) = api(
        "PATCH",
        format!("{}/api/admin/prims/{ball}", base(&handle)),
        Some(tok.clone()),
        Some(serde_json::json!({
            "latitude": spot.0, "longitude": spot.1, "altitude": 0.11
        })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    // An edit holds a physical prim still for a moment before it is live again.
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    // A player half a metre behind it, facing north (yaw π faces -z), kicks.
    let mut player = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    let behind = (spot.0 - 0.5 / m_lat, spot.1);
    let intent = |emote| NetMessage::ClientIntent {
        request_id: 0,
        move_x: 0.0,
        move_z: 0.0,
        display_yaw: std::f32::consts::PI,
        flying: false,
        fly_up: false,
        fly_down: false,
        jump: false,
        emote,
        seated: None,
        geo: Some(GeoAnchor {
            latitude: behind.0,
            longitude: behind.1,
        }),
    };
    for msg in [intent(None), intent(Some(cartopolis_core::Emote::Kick))] {
        player
            .send(Bytes::from(encode_app_frame(&msg).unwrap()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    let call = recv_until(&mut player, "the referee to call it", |m| match m {
        NetMessage::ChatMessage { body, .. } if body.contains("GOAL") => Some(body),
        _ => None,
    })
    .await;
    assert!(call.contains("north end"), "{call}");
    assert!(call.contains("North 0 – 1 South"), "{call}");
}

// ---------------------------------------------------------------------------
// Roles and permissions (`docs/notes/permissions.md`)
// ---------------------------------------------------------------------------

/// The id of an account, from the operator's list.
async fn account_id(handle: &SimHandle, admin: &str, username: &str) -> String {
    let (status, body) = api(
        "GET",
        format!("{}/api/admin/accounts", base(handle)),
        Some(admin.to_string()),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["username"] == username)
        .unwrap_or_else(|| panic!("no account {username}: {body}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Give `username` a role holding `permissions`, as an operator does from the
/// web UI.
async fn grant(handle: &SimHandle, admin: &str, username: &str, role: &str, permissions: &[&str]) {
    let (status, body) = api(
        "POST",
        format!("{}/api/admin/roles", base(handle)),
        Some(admin.to_string()),
        Some(serde_json::json!({ "id": role, "name": role, "permissions": permissions })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let id = account_id(handle, admin, username).await;
    let (status, body) = api(
        "PUT",
        format!("{}/api/admin/accounts/{id}/roles", base(handle)),
        Some(admin.to_string()),
        Some(serde_json::json!({ "roles": [role] })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

/// A role widens what one account may do, and it reaches a connection that is
/// already open: the client is told (`Permissions`) and the connection's own
/// gate follows, with no reconnect.
#[tokio::test]
async fn a_role_lets_one_account_edit_anybodys_prims() {
    let (handle, _dir) = server().await;
    let admin = token(&handle).await;
    let mut walker = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    let lamp = build(&mut walker, "Walkers lamp", LAT, LNG).await;

    let mut meddler = connect(&handle.local_addr.to_string(), "meddler", "meddlerpw").await;
    let refused = try_rename(&mut meddler, lamp, "Mine now").await;
    assert_eq!(refused.map(|r| r.0), Some(403));

    grant(&handle, &admin, "meddler", "caretaker", &["prim_edit_any"]).await;
    let granted = recv_until(&mut meddler, "the new permissions", |m| match m {
        NetMessage::Permissions { granted } if granted.iter().any(|p| p == "prim_edit_any") => {
            Some(granted)
        }
        _ => None,
    })
    .await;
    assert!(granted.iter().any(|p| p == "prim_create"), "{granted:?}");
    assert_eq!(try_rename(&mut meddler, lamp, "Tidied").await, None);

    // And back: taking the role away closes the gate on the same connection.
    let id = account_id(&handle, &admin, "meddler").await;
    let (status, body) = api(
        "PUT",
        format!("{}/api/admin/accounts/{id}/roles", base(&handle)),
        Some(admin.clone()),
        Some(serde_json::json!({ "roles": [] })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    recv_until(&mut meddler, "the narrowed permissions", |m| match m {
        NetMessage::Permissions { granted } if !granted.iter().any(|p| p == "prim_edit_any") => {
            Some(())
        }
        _ => None,
    })
    .await;
    let refused = try_rename(&mut meddler, lamp, "Mine again").await;
    assert_eq!(refused.map(|r| r.0), Some(403));
}

/// A signed-in client is told what it may do, so it can offer only that.
#[tokio::test]
async fn a_client_is_told_its_permissions_at_sign_in() {
    let (handle, _dir) = server().await;
    let mut walker = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    let granted = recv_until(&mut walker, "permissions", |m| match m {
        NetMessage::Permissions { granted } => Some(granted),
        _ => None,
    })
    .await;
    // The player role: building, but not replacing buildings.
    assert!(granted.iter().any(|p| p == "prim_create"), "{granted:?}");
    assert!(
        !granted.iter().any(|p| p == "building_override"),
        "{granted:?}"
    );
}

/// Narrowing the player role narrows every account at once, and the refusal
/// says it is the account's permissions, not somebody else's prim.
#[tokio::test]
async fn narrowing_the_player_role_stops_everyone_building() {
    let (handle, _dir) = server().await;
    let admin = token(&handle).await;
    let (status, body) = api(
        "PATCH",
        format!("{}/api/admin/roles/player", base(&handle)),
        Some(admin.clone()),
        Some(serde_json::json!({ "permissions": ["prim_edit_own"] })),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let mut walker = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    let msg = NetMessage::CreatePrim {
        request_id: 5,
        name: "nope".into(),
        position: glam::Vec3::new(0.0, 2.0, 0.0),
        rotation: glam::Vec3::ZERO,
        scale: glam::Vec3::ONE,
        color: [0.5, 0.5, 0.5],
        texture_id: None,
        shape: "box".into(),
        surface: cartopolis_core::PrimSurface::default(),
        geometry: cartopolis_core::PrimGeometry::default(),
        geo: GeoAnchor {
            latitude: LAT,
            longitude: LNG,
        },
        particles: None,
        model: None,
    };
    walker
        .send(Bytes::from(encode_app_frame(&msg).unwrap()))
        .await
        .unwrap();
    let (code, message) = recv_until(&mut walker, "a refusal", |m| match m {
        NetMessage::ServerError {
            request_id: 5,
            code,
            message,
        } => Some((code, message)),
        _ => None,
    })
    .await;
    assert_eq!(code, 403);
    assert!(message.contains("may not build"), "{message}");
}

/// Ask for a prim's script, as the editor does when it opens.
async fn ask_script(c: &mut Client, prim_id: i64) {
    let msg = NetMessage::GetPrimScript {
        request_id: 9,
        prim_id,
    };
    c.send(Bytes::from(encode_app_frame(&msg).unwrap()))
        .await
        .unwrap();
}

/// Without `script_read_any`, a script is its owner's to read.
#[tokio::test]
async fn a_script_is_private_without_script_read_any() {
    let (handle, _dir) = server().await;
    let admin = token(&handle).await;
    let (status, _) = api(
        "PATCH",
        format!("{}/api/admin/roles/player", base(&handle)),
        Some(admin.clone()),
        Some(serde_json::json!({
            "permissions": ["prim_create", "prim_edit_own", "script_edit_own"]
        })),
    )
    .await;
    assert_eq!(status, 200);
    let mut walker = connect(&handle.local_addr.to_string(), "walker", "walkerpw").await;
    let lamp = build(&mut walker, "Secret lamp", LAT, LNG).await;
    set_script(&mut walker, lamp, "function on_touch() say('hi') end").await;

    let mut meddler = connect(&handle.local_addr.to_string(), "meddler", "meddlerpw").await;
    ask_script(&mut meddler, lamp).await;
    let answer = recv_until(&mut meddler, "an answer", |m| match m {
        NetMessage::ServerError {
            request_id: 9,
            code,
            ..
        } => Some(Err(code)),
        NetMessage::PrimScript { source, .. } => Some(Ok(source)),
        _ => None,
    })
    .await;
    assert_eq!(answer, Err(403));

    ask_script(&mut walker, lamp).await;
    let mine = recv_until(&mut walker, "the source", |m| match m {
        NetMessage::PrimScript { source, .. } => Some(source),
        _ => None,
    })
    .await;
    assert!(mine.contains("say('hi')"));
}

/// A role may manage accounts, but not make anybody an admin — or that role
/// would be a way to become one — and the operator UI opens to it.
#[tokio::test]
async fn managing_accounts_is_not_making_admins() {
    let (handle, _dir) = server().await;
    let admin = token(&handle).await;

    // A plain player is no operator: the admin login answers as for a wrong
    // password.
    let (status, _) = api(
        "POST",
        format!("{}/api/admin/login", base(&handle)),
        None,
        Some(serde_json::json!({"username": "walker", "password": "walkerpw"})),
    )
    .await;
    assert_eq!(status, 401);

    grant(&handle, &admin, "walker", "desk", &["accounts_manage"]).await;
    let (status, body) = api(
        "POST",
        format!("{}/api/admin/login", base(&handle)),
        None,
        Some(serde_json::json!({"username": "walker", "password": "walkerpw"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let desk = body["token"].as_str().unwrap().to_string();

    let (status, me) = api(
        "GET",
        format!("{}/api/admin/me", base(&handle)),
        Some(desk.clone()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(me["admin"], false);
    assert!(me["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "accounts_manage"));

    let walker_id = account_id(&handle, &desk, "walker").await;
    let (status, _) = api(
        "PATCH",
        format!("{}/api/admin/accounts/{walker_id}", base(&handle)),
        Some(desk.clone()),
        Some(serde_json::json!({ "admin": true })),
    )
    .await;
    assert_eq!(
        status, 403,
        "a role that manages accounts made itself admin"
    );

    // Nor may it give itself more: roles are their own permission.
    let (status, _) = api(
        "POST",
        format!("{}/api/admin/roles", base(&handle)),
        Some(desk),
        Some(serde_json::json!({"id": "all", "name": "All", "permissions": ["roles_manage"]})),
    )
    .await;
    assert_eq!(status, 403);
}
