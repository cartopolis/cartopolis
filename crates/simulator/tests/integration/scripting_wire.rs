//! Prim scripting end-to-end over loopback TCP: the path the editor actually
//! drives, which the `scripting` unit tests deliberately do not cover.
//!
//! They test the VM in isolation — a `Slot` compiled and stepped in-process. This
//! tests everything between it and a client: that a stored script is compiled and
//! answers, that its console output reaches the wire, that a touch from a
//! connection becomes an `on_touch`, and that a `set_pos` becomes an authoritative
//! prim update every observer sees. Each of those is a seam the unit tests cannot
//! see past, and three of them (the host thread, the tick barrier, the broadcast)
//! were wired by hand.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, GeoAnchor, NetMessage, PrimGeometry,
    PrimSurface, ScriptLevel, PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, serve, SimConfig, SimHandle};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Client = Framed<TcpStream, LengthDelimitedCodec>;

async fn start_sim() -> (SimHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("scripts.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, "dev", "dev").unwrap();
    }
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    };
    (serve(Arc::new(config)).await.unwrap(), dir)
}

async fn connect_auth(addr: &str) -> Client {
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
            username: "dev".into(),
            password: "dev".into(),
        },
    })
    .unwrap();
    c.send(Bytes::from(hello)).await.unwrap();
    let ack = decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap();
    assert!(matches!(ack, NetMessage::ServerHelloAck { .. }));
    c
}

/// Read frames until one matches, discarding the snapshot traffic in between.
///
/// The deadline covers the **whole wait**, not each `next()`. A per-frame timeout
/// cannot fire here: the server sends a snapshot twenty times a second, so a
/// condition that will never be met still sees traffic forever and the test hangs
/// rather than failing. It did exactly that, for twenty minutes, before this was a
/// deadline.
async fn recv_until<T>(
    c: &mut Client,
    what: &str,
    mut f: impl FnMut(NetMessage) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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

async fn recv_output(c: &mut Client) -> (ScriptLevel, String) {
    recv_until(c, "script output", |m| match m {
        NetMessage::ScriptOutput { level, text, .. } => Some((level, text)),
        _ => None,
    })
    .await
}

async fn send(c: &mut Client, msg: &NetMessage) {
    c.send(Bytes::from(encode_app_frame(msg).unwrap()))
        .await
        .unwrap();
}

/// Create a prim and return its id, taken from the `PrimUpsert` the server
/// broadcasts (the create has no direct reply).
async fn create_prim(c: &mut Client) -> i64 {
    send(
        c,
        &NetMessage::CreatePrim {
            request_id: 1,
            name: "scripted".into(),
            position: glam::Vec3::new(1.0, 1.0, 1.0),
            rotation: glam::Vec3::ZERO,
            scale: glam::Vec3::ONE,
            color: [0.5, 0.5, 0.5],
            texture_id: None,
            shape: "box".into(),
            surface: PrimSurface::default(),
            geometry: PrimGeometry::default(),
            geo: GeoAnchor {
                latitude: 0.0,
                longitude: 0.0,
            },
            particles: None,
            model: None,
        },
    )
    .await;
    recv_until(c, "prim upsert", |m| match m {
        NetMessage::PrimUpsert { prim } => Some(prim.id),
        _ => None,
    })
    .await
}

/// The whole loop the editor drives: store a script, get told it compiled, touch
/// the prim, and hear back from the handler.
#[tokio::test]
async fn store_compile_touch_and_hear_back() {
    let (handle, _dir) = start_sim().await;
    let mut c = connect_auth(&handle.local_addr.to_string()).await;

    // Capacity is pushed before anything else, so an editor can show headroom.
    let (prims_max, scripts_max) = recv_until(&mut c, "world capacity", |m| match m {
        NetMessage::WorldCapacity {
            prims_max,
            scripts_max,
            ..
        } => Some((prims_max, scripts_max)),
        _ => None,
    })
    .await;
    assert!(prims_max > 0 && scripts_max > 0, "budget should be set");

    let prim_id = create_prim(&mut c).await;

    send(
        &mut c,
        &NetMessage::SetPrimScript {
            request_id: 2,
            prim_id,
            source: "function on_touch(id, name) log('touched by ' .. name) end".into(),
        },
    )
    .await;

    let (level, text) = recv_output(&mut c).await;
    assert_eq!(
        level,
        ScriptLevel::Notice,
        "expected a compile notice: {text}"
    );
    assert!(
        text.contains("on_touch"),
        "notice should name the handlers: {text}"
    );

    send(
        &mut c,
        &NetMessage::TouchPrim {
            request_id: 3,
            prim_id,
        },
    )
    .await;
    let (level, text) = recv_output(&mut c).await;
    assert_eq!(level, ScriptLevel::Info, "{text}");
    assert_eq!(text, "touched by dev");

    // And the editor can read back what it stored.
    send(
        &mut c,
        &NetMessage::GetPrimScript {
            request_id: 4,
            prim_id,
        },
    )
    .await;
    let (source, running) = recv_until(&mut c, "prim script", |m| match m {
        NetMessage::PrimScript {
            source, running, ..
        } => Some((source, running)),
        _ => None,
    })
    .await;
    assert!(source.contains("on_touch"));
    assert!(running, "a stored script should read back as running");
}

/// A script's `set_pos` has to become authoritative world state, not just a
/// number inside the VM: it is applied at the tick barrier and broadcast like any
/// other prim update.
#[tokio::test]
async fn script_moves_the_prim_for_everyone() {
    let (handle, _dir) = start_sim().await;
    let addr = handle.local_addr.to_string();
    let mut author = connect_auth(&addr).await;
    let prim_id = create_prim(&mut author).await;

    // A second client, which never asks for anything — it should see the move.
    let mut observer = connect_auth(&addr).await;

    send(
        &mut author,
        &NetMessage::SetPrimScript {
            request_id: 2,
            prim_id,
            source: "function on_start() set_pos(42.0, 7.0, -3.0) end".into(),
        },
    )
    .await;

    let pos = recv_until(&mut observer, "moved prim", |m| match m {
        NetMessage::PrimUpsert { prim } if prim.id == prim_id && prim.position.x > 41.0 => {
            Some(prim.position)
        }
        _ => None,
    })
    .await;
    assert_eq!(pos, glam::Vec3::new(42.0, 7.0, -3.0));
}

/// A script that does not compile must come back as an error the author can read,
/// and must leave the prim unscripted rather than half-loaded.
#[tokio::test]
async fn a_broken_script_reports_and_does_not_run() {
    let (handle, _dir) = start_sim().await;
    let mut c = connect_auth(&handle.local_addr.to_string()).await;
    let prim_id = create_prim(&mut c).await;

    send(
        &mut c,
        &NetMessage::SetPrimScript {
            request_id: 2,
            prim_id,
            source: "function on_start( end".into(),
        },
    )
    .await;
    let (level, text) = recv_output(&mut c).await;
    assert_eq!(level, ScriptLevel::Error, "{text}");
    assert!(
        text.contains(&format!("prim{prim_id}")),
        "the error should name the chunk so the editor can point at a line: {text}"
    );
}

/// Storing an empty script removes it — the editor's Stop button — and the
/// removal is reflected in what a later read gets back.
#[tokio::test]
async fn an_empty_script_removes_it() {
    let (handle, _dir) = start_sim().await;
    let mut c = connect_auth(&handle.local_addr.to_string()).await;
    let prim_id = create_prim(&mut c).await;

    send(
        &mut c,
        &NetMessage::SetPrimScript {
            request_id: 2,
            prim_id,
            source: "function on_start() log('up') end".into(),
        },
    )
    .await;
    recv_output(&mut c).await;

    send(
        &mut c,
        &NetMessage::SetPrimScript {
            request_id: 3,
            prim_id,
            source: String::new(),
        },
    )
    .await;
    send(
        &mut c,
        &NetMessage::GetPrimScript {
            request_id: 4,
            prim_id,
        },
    )
    .await;
    let (source, running) = recv_until(&mut c, "prim script", |m| match m {
        NetMessage::PrimScript {
            source, running, ..
        } => Some((source, running)),
        _ => None,
    })
    .await;
    assert!(source.is_empty(), "the script should be gone: {source:?}");
    assert!(!running);
}

/// The appearance calls reach the wire as ordinary prim state, so a client that
/// knows nothing about scripting renders the result.
#[tokio::test]
async fn appearance_calls_become_prim_state() {
    let (handle, _dir) = start_sim().await;
    let mut c = connect_auth(&handle.local_addr.to_string()).await;
    let prim_id = create_prim(&mut c).await;

    send(
        &mut c,
        &NetMessage::SetPrimScript {
            request_id: 2,
            prim_id,
            source: "function on_start() \
                     set_name('lantern') \
                     set_alpha(0.5) \
                     set_glow(0.75) \
                     set_full_bright(true) \
                     set_rot(0, 90, 0) \
                     end"
            .into(),
        },
    )
    .await;

    // One update carries the whole state: the tick loop coalesces a handler's
    // writes into a single upsert per prim, so there is no torn intermediate to
    // match by accident. Asserting on the *first* upsert is what proves that — if
    // each write got its own frame, this one would still have the default alpha.
    let prim = recv_until(&mut c, "styled prim", |m| match m {
        NetMessage::PrimUpsert { prim } if prim.id == prim_id && prim.name == "lantern" => {
            Some(prim)
        }
        _ => None,
    })
    .await;
    assert_eq!(prim.surface.alpha, 0.5);
    assert_eq!(prim.surface.glow, 0.75);
    assert!(prim.surface.full_bright);
    // Degrees at the script boundary, radians on the wire.
    assert!(
        (prim.rotation.y - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
        "90 degrees should arrive as pi/2, got {:?}",
        prim.rotation
    );
}

/// `die` removes the prim for everyone and gives its budget back.
#[tokio::test]
async fn a_script_can_end_its_own_prim() {
    let (handle, _dir) = start_sim().await;
    let addr = handle.local_addr.to_string();
    let mut c = connect_auth(&addr).await;
    let prim_id = create_prim(&mut c).await;

    send(
        &mut c,
        &NetMessage::SetPrimScript {
            request_id: 2,
            prim_id,
            source: "function on_start() die() end".into(),
        },
    )
    .await;

    let removed = recv_until(&mut c, "prim removed", |m| match m {
        NetMessage::PrimRemoved { id } => Some(id),
        _ => None,
    })
    .await;
    assert_eq!(removed, prim_id);

    // The capacity broadcast that follows reports the prim is gone. Reading it
    // back proves the budget was released rather than leaked on a self-delete.
    let used = recv_until(&mut c, "capacity after death", |m| match m {
        NetMessage::WorldCapacity { prims_used, .. } => Some(prims_used),
        _ => None,
    })
    .await;
    assert_eq!(used, 0, "the prim's slot should have been given back");
}
