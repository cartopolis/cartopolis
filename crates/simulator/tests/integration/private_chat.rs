//! Private chats end to end: real MLS devices (`cartopolis_e2ee`) talking
//! through the real server over loopback TCP (protocol 25).
//!
//! The unit tests in `src/e2ee.rs` pin the server's rules against a database;
//! these pin that the rules, the wire and actual encrypting clients fit
//! together — and that what crosses the server is sealed.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, E2eeCursor, E2eeWelcome, NetMessage,
    PROTOCOL_VERSION,
};
use cartopolis_e2ee::{Device, Payload, Received};
use cartopolis_simulator::{ensure_account, serve, SimConfig};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use uuid::Uuid;

pub(super) type Client = Framed<TcpStream, LengthDelimitedCodec>;

pub(super) const PATIENT: Duration = Duration::from_secs(3);
const QUIET: Duration = Duration::from_millis(700);

struct Server {
    addr: String,
    _dir: tempfile::TempDir,
    _handle: cartopolis_simulator::SimHandle,
}

async fn start(users: &[&str]) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("e2ee.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        for u in users {
            ensure_account(&conn, u, "pw").unwrap();
        }
    }
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    Server {
        addr: handle.local_addr.to_string(),
        _dir: dir,
        _handle: handle,
    }
}

pub(super) async fn connect(addr: &str, username: &str) -> Client {
    let stream = TcpStream::connect(addr).await.unwrap();
    let mut c = Framed::new(
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
                username: username.into(),
                password: "pw".into(),
            },
        },
    )
    .await;
    let ack = decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap();
    assert!(matches!(ack, NetMessage::ServerHelloAck { .. }), "{ack:?}");
    c
}

pub(super) async fn send(c: &mut Client, msg: &NetMessage) {
    c.send(Bytes::from(encode_app_frame(msg).unwrap()))
        .await
        .unwrap();
}

/// Next frame matching `f`, with its raw bytes, or `None` after `window`.
pub(super) async fn next_matching<T>(
    c: &mut Client,
    window: Duration,
    mut f: impl FnMut(NetMessage) -> Option<T>,
) -> Option<(T, Vec<u8>)> {
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
            return Some((v, frame.to_vec()));
        }
    }
}

/// Connect, register a device, and wait for the server to say it has.
pub(super) async fn online(addr: &str, dev: &mut Device) -> Client {
    let name = dev.identity().username.clone();
    let mut c = connect(addr, &name).await;
    send(
        &mut c,
        &NetMessage::E2eeRegisterDevice {
            request_id: 1,
            device: dev.identity().device,
            key_packages: dev.key_packages(3).unwrap(),
        },
    )
    .await;
    next_matching(&mut c, PATIENT, |m| match m {
        NetMessage::E2eeDeviceStatus { key_packages, .. } => Some(key_packages),
        _ => None,
    })
    .await
    .expect("device registered");
    c
}

/// Alice starts a direct chat with Bob and invites his device. Returns the
/// chat's id; Alice's side is confirmed, Bob has an invitation on its way.
pub(super) async fn start_direct(a: &mut Client, alice: &mut Device, with: &str) -> Vec<u8> {
    send(
        a,
        &NetMessage::E2eeClaimKeyPackages {
            request_id: 2,
            username: with.into(),
        },
    )
    .await;
    let (packages, _) = next_matching(a, PATIENT, |m| match m {
        NetMessage::E2eeKeyPackages { packages, .. } => Some(packages),
        _ => None,
    })
    .await
    .expect("key packages");
    assert_eq!(packages.len(), 1, "one per device of the person asked for");

    let group = alice.create_group().unwrap();
    send(
        a,
        &NetMessage::E2eeCreateGroup {
            request_id: 3,
            group: group.clone(),
            direct_with: Some(with.into()),
        },
    )
    .await;
    let (existing, _) = next_matching(a, PATIENT, |m| match m {
        NetMessage::E2eeGroupCreated { existing, .. } => Some(existing),
        _ => None,
    })
    .await
    .expect("group registered");
    assert_eq!(existing, None);

    let bytes: Vec<Vec<u8>> = packages.iter().map(|p| p.bytes.clone()).collect();
    let add = alice.add(&group, &bytes).unwrap();
    send(
        a,
        &NetMessage::E2eeSend {
            request_id: 4,
            group: group.clone(),
            epoch: add.epoch,
            commit: true,
            message: add.message,
            welcome: Some(E2eeWelcome {
                devices: packages.iter().map(|p| p.device).collect(),
                members: vec![with.into()],
                bytes: add.welcome.unwrap(),
            }),
        },
    )
    .await;
    let (seq, _) = next_matching(a, PATIENT, |m| match m {
        NetMessage::E2eeSent { seq, refused, .. } => Some((seq, refused)),
        _ => None,
    })
    .await
    .expect("commit answered");
    assert_eq!(seq, (Some(1), None));
    alice.confirm_pending(&group).unwrap();
    group
}

async fn say(c: &mut Client, dev: &mut Device, group: &[u8], body: &str) -> u64 {
    let out = dev
        .encrypt(
            group,
            &Payload::Text {
                body: body.into(),
                sent_at_ms: 0,
            },
        )
        .unwrap();
    send(
        c,
        &NetMessage::E2eeSend {
            request_id: 9,
            group: group.to_vec(),
            epoch: out.epoch,
            commit: false,
            message: out.message,
            welcome: None,
        },
    )
    .await;
    let ((seq, refused), _) = next_matching(c, PATIENT, |m| match m {
        NetMessage::E2eeSent { seq, refused, .. } => Some((seq, refused)),
        _ => None,
    })
    .await
    .expect("send answered");
    assert_eq!(refused, None);
    seq.unwrap()
}

fn text(dev: &mut Device, group: &[u8], message: &[u8]) -> String {
    match dev.receive(group, message).unwrap() {
        Received::Message { bytes, .. } => match Payload::decode(&bytes).unwrap() {
            Payload::Text { body, .. } => body,
            other => panic!("expected text, got {other:?}"),
        },
        other => panic!("expected a message, got {other:?}"),
    }
}

#[tokio::test]
async fn two_people_talk_privately_and_the_server_only_ever_holds_sealed_bytes() {
    let s = start(&["alice", "bob"]).await;
    let mut alice = Device::new("alice", Uuid::new_v4()).unwrap();
    let mut bob = Device::new("bob", Uuid::new_v4()).unwrap();
    let mut a = online(&s.addr, &mut alice).await;
    let mut b = online(&s.addr, &mut bob).await;

    let group = start_direct(&mut a, &mut alice, "bob").await;

    // Bob is online, so the invitation arrives live.
    let ((after, welcome), _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeInvite { after, welcome, .. } => Some((after, welcome)),
        _ => None,
    })
    .await
    .expect("invitation delivered");
    assert_eq!(bob.join(&welcome).unwrap(), group);
    assert_eq!(after, 1);

    let secret = "the key is under the third flowerpot";
    let seq = say(&mut a, &mut alice, &group, secret).await;
    let ((message, from), raw) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeDeliver {
            message,
            sender,
            seq: s,
            ..
        } if s == seq => Some((message, sender)),
        _ => None,
    })
    .await
    .expect("message delivered");
    assert_eq!(from, "alice");
    assert!(
        !raw.windows(secret.len()).any(|w| w == secret.as_bytes()),
        "the frame the server relayed must not contain the plaintext"
    );
    assert_eq!(text(&mut bob, &group, &message), secret);
}

#[tokio::test]
async fn a_message_sent_while_you_were_away_arrives_on_the_next_sync() {
    let s = start(&["alice", "bob"]).await;
    let mut alice = Device::new("alice", Uuid::new_v4()).unwrap();
    let mut bob = Device::new("bob", Uuid::new_v4()).unwrap();
    let mut a = online(&s.addr, &mut alice).await;
    // Bob registers his device once, then goes offline before anything happens.
    drop(online(&s.addr, &mut bob).await);

    let group = start_direct(&mut a, &mut alice, "bob").await;
    say(&mut a, &mut alice, &group, "call me when you're back").await;

    let mut b = online(&s.addr, &mut bob).await;
    // First sync: no chats joined yet, so only the waiting invitation.
    send(
        &mut b,
        &NetMessage::E2eeSync {
            request_id: 5,
            cursors: vec![],
        },
    )
    .await;
    let ((after, welcome), _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeInvite { after, welcome, .. } => Some((after, welcome)),
        _ => None,
    })
    .await
    .expect("stored invitation");
    bob.join(&welcome).unwrap();

    // Second sync, now reading from after the commit that added him.
    send(
        &mut b,
        &NetMessage::E2eeSync {
            request_id: 6,
            cursors: vec![E2eeCursor {
                group: group.clone(),
                after,
            }],
        },
    )
    .await;
    let (message, _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeDeliver { message, .. } => Some(message),
        _ => None,
    })
    .await
    .expect("stored message");
    assert_eq!(text(&mut bob, &group, &message), "call me when you're back");
    // And the replay says where it ends, with the spent invitation not resent.
    let (again, _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeSynced { .. } => Some(false),
        NetMessage::E2eeInvite { .. } => Some(true),
        _ => None,
    })
    .await
    .expect("replay ends");
    assert!(!again, "a joined chat's invitation must not come back");
}

#[tokio::test]
async fn somebody_outside_the_chat_gets_nothing_from_it() {
    let s = start(&["alice", "bob", "eve"]).await;
    let mut alice = Device::new("alice", Uuid::new_v4()).unwrap();
    let mut bob = Device::new("bob", Uuid::new_v4()).unwrap();
    let mut eve = Device::new("eve", Uuid::new_v4()).unwrap();
    let mut a = online(&s.addr, &mut alice).await;
    let _b = online(&s.addr, &mut bob).await;
    let mut e = online(&s.addr, &mut eve).await;

    let group = start_direct(&mut a, &mut alice, "bob").await;
    say(&mut a, &mut alice, &group, "just between us").await;

    // Eve is online the whole time and hears nothing live…
    let live = next_matching(&mut e, QUIET, |m| match m {
        NetMessage::E2eeDeliver { .. } | NetMessage::E2eeInvite { .. } => Some(()),
        _ => None,
    })
    .await;
    assert!(live.is_none());
    // …and asking for the chat's log by id gets her nothing either.
    send(
        &mut e,
        &NetMessage::E2eeSync {
            request_id: 7,
            cursors: vec![E2eeCursor { group, after: 0 }],
        },
    )
    .await;
    let replay = next_matching(&mut e, PATIENT, |m| match m {
        NetMessage::E2eeDeliver { .. } => Some(true),
        NetMessage::E2eeSynced { .. } => Some(false),
        _ => None,
    })
    .await;
    assert_eq!(replay.map(|r| r.0), Some(false));
}

/// Starting a group asks for every member's keys at once, so a burst of claims
/// is allowed; past it the answer is a refusal that says so, not silence (an
/// empty answer would read as "they have no app that can chat privately").
#[tokio::test]
async fn a_groups_worth_of_claims_goes_through_and_a_flood_is_refused_out_loud() {
    let s = start(&["alice", "bob"]).await;
    let mut alice = Device::new("alice", Uuid::new_v4()).unwrap();
    let mut a = online(&s.addr, &mut alice).await;
    // Twice the burst, back to back. The bucket refills while they arrive, so
    // the exact split is not the point: a group's worth must get through, and
    // what does not must be refused with a reason rather than dropped.
    const SENT: u32 = 24;
    for i in 0..SENT {
        send(
            &mut a,
            &NetMessage::E2eeClaimKeyPackages {
                request_id: 100 + i,
                username: "bob".into(),
            },
        )
        .await;
    }
    let mut answered = 0u32;
    let mut refused = 0u32;
    while let Some((code, _)) = next_matching(&mut a, PATIENT, |m| match m {
        NetMessage::E2eeKeyPackages { .. } => Some(None),
        NetMessage::ServerError { code, .. } => Some(Some(code)),
        _ => None,
    })
    .await
    {
        match code {
            None => answered += 1,
            Some(429) => refused += 1,
            Some(other) => panic!("unexpected refusal code {other}"),
        }
        if answered + refused == SENT {
            break;
        }
    }
    assert_eq!(answered + refused, SENT, "every claim gets an answer");
    assert!(answered >= 12, "a group's worth goes through ({answered})");
    assert!(refused > 0, "a flood is refused, out loud");
}
