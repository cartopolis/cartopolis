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
//! * **`vfSetGeo` is durable horizontal motion.** `vfSetPos`'s x and z are a
//!   client-frame echo (ADR-029), so this was the one thing a script could not do
//!   before protocol 20, and "the prim moved" has to mean the *anchor* moved.
//! * **`vfSensor` sees an avatar**, which needs the avatar's `ClientIntent` to
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

/// Groningen's Grote Markt — where the seeded region is anchored.
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
/// other thirteen. It also has to match `vfParticles`, or the same effect is
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
    // `vfParticleBurst` makes against `vfParticles`.
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
    // builds, and it is checked against their region's budget.
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

/// The whole demo, in the shape `content/firebird.lua` has it: a scripted prim
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
    // `content/firebird.lua`'s exactly.
    let source = r#"
        local LAT, LNG = 53.2194, 6.5665
        local RADIUS, LAP, NOTICE, SHAKE = 40.0, 4.0, 60.0, 0.4
        local M_LAT, M_LNG = vfGeoScale(LAT)
        local state, marked = "flying", 0

        function on_start()
            vfSetName("Firebird")
            vfParticles{ count = 64, life = 1.0 }
            vfSetTimerEvent(0.05)
        end

        function on_timer()
            local now = vfGetTime()
            if state == "flying" then
                local a = (now / LAP) * 2 * math.pi
                vfSetGeo(LAT + (RADIUS * math.cos(a)) / M_LAT,
                         LNG + (RADIUS * math.sin(a)) / M_LNG)
                local id, who = vfSensor(NOTICE)
                if id then
                    state, marked = "shaking", now
                    vfSetName("Firebird sees " .. who)
                end
            elseif state == "shaking" then
                local lat, lng = vfGetGeo()
                vfSetGeo(lat + 0.4 * math.sin(now * 47.0) / M_LAT, lng)
                if now - marked >= SHAKE then
                    state, marked = "blown", now
                    vfParticleBurst{ count = 200, life = 0.5 }
                    vfSetAlpha(0.0)
                end
            elseif state == "blown" and now - marked >= 0.3 then
                vfDie()
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
            "vfDie never removed the prim; it was last {body}"
        );
    }

    // And the region got its budget back — a prim that is gone from the world but
    // not from the counters is a region that fills up with nothing.
    let (_, regions) = api(
        "GET",
        format!("{}/api/admin/regions", base(&handle)),
        Some(tok),
        None,
    )
    .await;
    assert_eq!(
        regions["regions"][0]["prims_used"], 0,
        "the exploded prim should have released its budget: {regions}"
    );
}
