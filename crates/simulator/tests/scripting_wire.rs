//! Prim scripting end-to-end over loopback TCP: the path the editor actually
//! drives, which the `scripting` unit tests deliberately do not cover.
//!
//! They test the VM in isolation — a `Slot` compiled and stepped in-process. This
//! tests everything between it and a client: that a stored script is compiled and
//! answers, that its console output reaches the wire, that a touch from a
//! connection becomes an `on_touch`, and that a `vf.set_pos` becomes an authoritative
//! prim update every observer sees. Each of those is a seam the unit tests cannot
//! see past, and three of them (the host thread, the tick barrier, the broadcast)
//! were wired by hand.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use viberfox_core::{
    decode_app_frame, encode_app_frame, AuthCredential, GeoAnchor, NetMessage, PrimGeometry,
    PrimSurface, ScriptLevel, PROTOCOL_VERSION,
};
use viberfox_simulator::{ensure_account, serve, SimConfig, SimHandle};

type Client = Framed<TcpStream, LengthDelimitedCodec>;

async fn start_sim() -> (SimHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("scripts.db").to_str().unwrap().to_string();
    {
        let conn = viberfox_simulator::db::open_and_migrate(&db).unwrap();
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
async fn recv_until<T>(
    c: &mut Client,
    what: &str,
    mut f: impl FnMut(NetMessage) -> Option<T>,
) -> T {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), c.next())
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
async fn create_prim(c: &mut Client, region_id: i64) -> i64 {
    send(
        c,
        &NetMessage::CreatePrim {
            request_id: 1,
            region_id,
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
        },
    )
    .await;
    recv_until(c, "prim upsert", |m| match m {
        NetMessage::PrimUpsert { prim } => Some(prim.id),
        _ => None,
    })
    .await
}

async fn first_region(c: &mut Client) -> i64 {
    recv_until(c, "world snapshot", |m| match m {
        NetMessage::WorldSnapshot { regions, .. } => regions.first().map(|r| r.id),
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
    let (prims_max, scripts_max) = recv_until(&mut c, "region capacity", |m| match m {
        NetMessage::RegionCapacity {
            prims_max,
            scripts_max,
            ..
        } => Some((prims_max, scripts_max)),
        _ => None,
    })
    .await;
    assert!(prims_max > 0 && scripts_max > 0, "budget should be set");

    let region_id = first_region(&mut c).await;
    let prim_id = create_prim(&mut c, region_id).await;

    send(
        &mut c,
        &NetMessage::SetPrimScript {
            request_id: 2,
            prim_id,
            source: "function on_touch(id, name) vf.log('touched by ' .. name) end".into(),
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
    assert_eq!(level, ScriptLevel::Info);
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

/// A script's `vf.set_pos` has to become authoritative world state, not just a
/// number inside the VM: it is applied at the tick barrier and broadcast like any
/// other prim update.
#[tokio::test]
async fn script_moves_the_prim_for_everyone() {
    let (handle, _dir) = start_sim().await;
    let addr = handle.local_addr.to_string();
    let mut author = connect_auth(&addr).await;
    let region_id = first_region(&mut author).await;
    let prim_id = create_prim(&mut author, region_id).await;

    // A second client, which never asks for anything — it should see the move.
    let mut observer = connect_auth(&addr).await;

    send(
        &mut author,
        &NetMessage::SetPrimScript {
            request_id: 2,
            prim_id,
            source: "function on_start() vf.set_pos(42.0, 7.0, -3.0) end".into(),
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
    let region_id = first_region(&mut c).await;
    let prim_id = create_prim(&mut c, region_id).await;

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
    let region_id = first_region(&mut c).await;
    let prim_id = create_prim(&mut c, region_id).await;

    send(
        &mut c,
        &NetMessage::SetPrimScript {
            request_id: 2,
            prim_id,
            source: "function on_start() vf.log('up') end".into(),
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
