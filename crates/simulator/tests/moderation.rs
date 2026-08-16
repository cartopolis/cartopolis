//! The user-safety surface, end to end over loopback TCP.
//!
//! These four behaviours are what an App Store reviewer is looking for in an app
//! carrying user-generated content (guideline 1.2), plus in-app account deletion
//! (5.1.1(v)). They are tested at the wire because that is where the guarantee
//! lives: a client can be rewritten, so "the block is applied in the UI" is not a
//! block at all, and every one of these asserts that the *server* refuses.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, ChatScope, NetMessage, ReportReason,
    PROTOCOL_VERSION,
};
use cartopolis_simulator::{ensure_account, serve, SimConfig};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

type Client = Framed<TcpStream, LengthDelimitedCodec>;

struct Server {
    addr: String,
    db: String,
    _dir: tempfile::TempDir,
    _handle: cartopolis_simulator::SimHandle,
}

async fn start(users: &[(&str, &str)]) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("mod.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        for (u, p) in users {
            ensure_account(&conn, u, p).unwrap();
        }
    }
    let config = SimConfig {
        database_path: db.clone(),
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    Server {
        addr: handle.local_addr.to_string(),
        db,
        _dir: dir,
        _handle: handle,
    }
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

async fn send(c: &mut Client, msg: &NetMessage) {
    c.send(Bytes::from(encode_app_frame(msg).unwrap()))
        .await
        .unwrap();
}

/// Next frame matching `f`, or `None` if `window` passes without one.
///
/// A timeout is the *assertion* in half these tests — "the blocked line never
/// arrives" can only be tested by waiting — so it is short enough to keep the
/// suite quick and long enough to cover a loaded CI runner.
async fn next_matching<T>(
    c: &mut Client,
    window: Duration,
    mut f: impl FnMut(NetMessage) -> Option<T>,
) -> Option<T> {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        let Ok(Some(Ok(frame))) = tokio::time::timeout(left, c.next()).await else {
            return None;
        };
        if let Some(v) = f(decode_app_frame(&frame).unwrap()) {
            return Some(v);
        }
    }
}

fn chat_body(m: NetMessage) -> Option<String> {
    match m {
        NetMessage::ChatMessage { body, .. } => Some(body),
        _ => None,
    }
}

const QUIET: Duration = Duration::from_millis(700);
const PATIENT: Duration = Duration::from_secs(3);

/// Guideline 1.2's filter. Masked on the server, so it holds for every client and
/// for the stored scrollback — not just for the one that drew the line.
#[tokio::test]
async fn a_listed_word_is_masked_before_anybody_receives_it() {
    let s = start(&[("alice", "pw"), ("bob", "pw")]).await;
    let mut a = connect(&s.addr, "alice", "pw").await;
    let mut b = connect(&s.addr, "bob", "pw").await;

    send(
        &mut a,
        &NetMessage::ChatSend {
            request_id: 1,
            scope: ChatScope::Global,
            body: "you absolute retard, go away".into(),
        },
    )
    .await;

    let body = next_matching(&mut b, PATIENT, chat_body).await.unwrap();
    assert_eq!(body, "you absolute ******, go away");

    // And the stored copy is the masked one: a client joining later must not be
    // handed the original out of scrollback.
    let mut c = connect(&s.addr, "bob", "pw").await;
    let replayed = next_matching(&mut c, PATIENT, chat_body).await.unwrap();
    assert_eq!(replayed, "you absolute ******, go away");
}

/// Guideline 1.2's block, on the tier where it matters most: proximity, where the
/// person you blocked can still walk up to you.
#[tokio::test]
async fn a_blocked_speaker_is_not_delivered_and_the_block_survives_a_reconnect() {
    let s = start(&[("alice", "pw"), ("bob", "pw")]).await;
    let mut a = connect(&s.addr, "alice", "pw").await;
    let mut b = connect(&s.addr, "bob", "pw").await;

    // Bob blocks Alice, and gets his list back rather than an ack.
    send(
        &mut b,
        &NetMessage::SetBlocked {
            request_id: 1,
            username: "alice".into(),
            blocked: true,
        },
    )
    .await;
    let list = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::BlockedList { usernames } if !usernames.is_empty() => Some(usernames),
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(list, vec!["alice".to_string()]);

    for scope in [ChatScope::Global, ChatScope::Local { shout: false }] {
        send(
            &mut a,
            &NetMessage::ChatSend {
                request_id: 2,
                scope,
                body: "still here".into(),
            },
        )
        .await;
    }
    assert_eq!(
        next_matching(&mut b, QUIET, chat_body).await,
        None,
        "a blocked speaker was delivered"
    );

    // A block that only lasts as long as the session is not a block. Reconnect
    // and the stored global line must still be withheld from the replay.
    drop(b);
    let mut b2 = connect(&s.addr, "bob", "pw").await;
    let restored = next_matching(&mut b2, PATIENT, |m| match m {
        NetMessage::BlockedList { usernames } => Some(usernames),
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(restored, vec!["alice".to_string()]);
    assert_eq!(
        next_matching(&mut b2, QUIET, chat_body).await,
        None,
        "scrollback replayed a blocked speaker"
    );
}

/// Blocking yourself would take your own words out of every channel you speak
/// in, and read as the chat being broken.
#[tokio::test]
async fn blocking_yourself_is_refused() {
    let s = start(&[("alice", "pw")]).await;
    let mut a = connect(&s.addr, "alice", "pw").await;
    // Drain the login list first, so the assertion below cannot read it.
    next_matching(&mut a, PATIENT, |m| match m {
        NetMessage::BlockedList { usernames } => Some(usernames),
        _ => None,
    })
    .await
    .unwrap();

    send(
        &mut a,
        &NetMessage::SetBlocked {
            request_id: 1,
            username: "ALICE".into(),
            blocked: true,
        },
    )
    .await;
    assert_eq!(
        next_matching(&mut a, QUIET, |m| match m {
            NetMessage::BlockedList { usernames } => Some(usernames),
            _ => None,
        })
        .await,
        None,
        "self-block was accepted"
    );
}

/// Guideline 1.2's report path. The row is what an operator triages, so the test
/// asserts the row and not just the ack.
#[tokio::test]
async fn a_report_is_acknowledged_and_stored_for_the_operator() {
    let s = start(&[("alice", "pw"), ("bob", "pw")]).await;
    let mut b = connect(&s.addr, "bob", "pw").await;

    send(
        &mut b,
        &NetMessage::ReportUser {
            request_id: 9,
            username: "alice".into(),
            reason: ReportReason::Harassment,
            note: "followed me for ten minutes".into(),
            message: Some("the line as bob saw it".into()),
        },
    )
    .await;
    let accepted = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::ReportAck {
            request_id: 9,
            accepted,
        } => Some(accepted),
        _ => None,
    })
    .await
    .unwrap();
    assert!(accepted);

    let conn = cartopolis_simulator::db::open_and_migrate(&s.db).unwrap();
    let rows = cartopolis_simulator::db::list_reports(&conn, true, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject_username, "alice");
    assert_eq!(rows[0].reporter_username.as_deref(), Some("bob"));
    assert_eq!(rows[0].reason, "harassment");
    // The reported line is kept unmasked: masking is for the room, not for the
    // operator who has to decide what happened.
    assert_eq!(rows[0].message.as_deref(), Some("the line as bob saw it"));
    assert!(!rows[0].resolved);

    // Reporting yourself is refused rather than silently filed.
    send(
        &mut b,
        &NetMessage::ReportUser {
            request_id: 10,
            username: "bob".into(),
            reason: ReportReason::Other,
            note: String::new(),
            message: None,
        },
    )
    .await;
    let accepted = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::ReportAck {
            request_id: 10,
            accepted,
        } => Some(accepted),
        _ => None,
    })
    .await
    .unwrap();
    assert!(!accepted);
}

/// Guideline 5.1.1(v): deletion has to be reachable from inside the app, and it
/// has to actually delete.
#[tokio::test]
async fn deleting_your_own_account_needs_the_password_and_then_takes_effect() {
    let s = start(&[("alice", "pw"), ("keeper", "pw")]).await;
    let mut a = connect(&s.addr, "alice", "pw").await;

    // The wrong password is refused, and the session survives to try again.
    send(
        &mut a,
        &NetMessage::DeleteAccount {
            request_id: 1,
            password: "not it".into(),
        },
    )
    .await;
    let code = next_matching(&mut a, PATIENT, |m| match m {
        NetMessage::ServerError { code, .. } => Some(code),
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(code, 401);

    send(
        &mut a,
        &NetMessage::DeleteAccount {
            request_id: 2,
            password: "pw".into(),
        },
    )
    .await;
    assert!(
        next_matching(&mut a, PATIENT, |m| matches!(m, NetMessage::AccountDeleted)
            .then_some(()))
        .await
        .is_some()
    );

    // Gone from the store, and unable to sign in again.
    let conn = cartopolis_simulator::db::open_and_migrate(&s.db).unwrap();
    assert!(cartopolis_simulator::auth::lookup_account(&conn, "alice")
        .unwrap()
        .is_none());
    drop(conn);

    let stream = TcpStream::connect(&s.addr).await.unwrap();
    let mut c: Client = Framed::new(
        stream,
        LengthDelimitedCodec::builder()
            .max_frame_length(32 << 20)
            .little_endian()
            .new_codec(),
    );
    send(
        &mut c,
        &NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: AuthCredential::Password {
                username: "alice".into(),
                password: "pw".into(),
            },
        },
    )
    .await;
    let code = next_matching(&mut c, PATIENT, |m| match m {
        NetMessage::ServerError { code, .. } => Some(code),
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(code, 401, "a deleted account signed back in");
}
