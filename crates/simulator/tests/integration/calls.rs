//! Calls end to end (protocol 26): real MLS devices place a call through the
//! real server — signalling over its TCP connection, media over UDP to its
//! forwarder — and what the forwarder handles is sealed.

use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cartopolis_calls::keys::{CallKeys, BASE_KEY_LEN};
use cartopolis_calls::link::{Leg, Link, Output};
use cartopolis_calls::media::{Codec, Kind, ParticipantId, TrackInfo};
use cartopolis_core::{CallTrackKind, NetMessage};
use cartopolis_e2ee::Device;
use cartopolis_simulator::{ensure_account, serve, SimConfig, SimHandle};
use futures_util::StreamExt;
use uuid::Uuid;

use super::private_chat::{next_matching, online, send, start_direct, Client, PATIENT};

async fn start(users: &[&str]) -> (SimHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("calls.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        for u in users {
            ensure_account(&conn, u, "pw").unwrap();
        }
        // Everyone a contact of everyone: a call rides a private chat, and a
        // chat with somebody who is not a contact starts as a request
        // (protocol 27, `contacts`).
        conn.execute(
            "INSERT INTO contacts (account, contact, since_ms)
             SELECT a.id, b.id, 0 FROM accounts a, accounts b WHERE a.id <> b.id",
            [],
        )
        .unwrap();
    }
    let config = SimConfig {
        database_path: db,
        listen: "127.0.0.1:0".into(),
        calls_listen: Some("127.0.0.1:0".into()),
        ..SimConfig::default()
    };
    (serve(Arc::new(config)).await.unwrap(), dir)
}

/// One device in a call: its link, its keys, and what it heard.
struct Phone {
    link: Link,
    keys: CallKeys,
    up: UdpSocket,
    down: UdpSocket,
    heard: Vec<(ParticipantId, Vec<u8>)>,
}

fn socket() -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    s.set_nonblocking(true).unwrap();
    s
}

/// Ask to join `group`'s call; returns the phone on success, the refusal otherwise.
async fn join(c: &mut Client, dev: &Device, group: &[u8]) -> Result<(Phone, u64), (u32, String)> {
    let (up, down) = (socket(), socket());
    let (mut link, offer) = Link::new(
        Instant::now(),
        up.local_addr().unwrap(),
        down.local_addr().unwrap(),
        false,
        false,
    )
    .unwrap();
    send(
        c,
        &NetMessage::CallJoin {
            request_id: 40,
            group: group.to_vec(),
            offer,
            screen: None,
        },
    )
    .await;
    let (answer, _) = next_matching(c, PATIENT, |m| match m {
        NetMessage::CallJoined {
            participant,
            answer,
            ..
        } => Some(Ok((participant, answer))),
        NetMessage::ServerError {
            request_id: 40,
            code,
            message,
        } => Some(Err((code, message))),
        _ => None,
    })
    .await
    .expect("join answered");
    let (participant, answer) = answer?;
    link.accept_answer(&answer).unwrap();
    let e = dev.call_epoch(group, BASE_KEY_LEN).unwrap();
    let mut keys = CallKeys::new();
    keys.set_epoch(
        e.epoch,
        e.base_key,
        e.own_index,
        e.members.iter().map(|m| m.0).collect(),
    );
    Ok((
        Phone {
            link,
            keys,
            up,
            down,
            heard: Vec::new(),
        },
        participant,
    ))
}

/// Everything waiting on a client's TCP connection that a call cares about:
/// answer the forwarder's offers, and note any roster.
async fn signalling(
    c: &mut Client,
    phone: &mut Phone,
    group: &[u8],
    rosters: &mut Vec<Vec<String>>,
) {
    for m in signalling_all(c, phone, group).await {
        if let NetMessage::CallRoster { members, .. } = m {
            rosters.push(members.into_iter().map(|m| m.username).collect());
        }
    }
}

/// [`signalling`], returning every other message that arrived.
async fn signalling_all(c: &mut Client, phone: &mut Phone, group: &[u8]) -> Vec<NetMessage> {
    let mut rest = Vec::new();
    while let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_millis(1), c.next()).await {
        match cartopolis_core::decode_app_frame(&frame).unwrap() {
            NetMessage::CallOffer { sdp, tracks, .. } => {
                let tracks: Vec<TrackInfo> = tracks
                    .into_iter()
                    .map(|t| TrackInfo {
                        mid: t.mid,
                        from: ParticipantId(t.participant),
                        kind: match t.kind {
                            CallTrackKind::Audio => Kind::Audio,
                            CallTrackKind::Camera => Kind::Video,
                            CallTrackKind::Screen => Kind::Screen,
                        },
                    })
                    .collect();
                let answer = phone
                    .link
                    .accept_offer(std::time::Instant::now(), &sdp, &tracks)
                    .unwrap();
                send(
                    c,
                    &NetMessage::CallAnswer {
                        group: group.to_vec(),
                        sdp: answer,
                    },
                )
                .await;
            }
            other => rest.push(other),
        }
    }
    rest
}

fn pump(phone: &mut Phone) {
    let now = Instant::now();
    let mut buf = [0u8; 2048];
    for (leg, sock) in [(Leg::Up, &phone.up), (Leg::Down, &phone.down)] {
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            phone.link.receive(now, leg, from, &buf[..n]);
        }
    }
    let (outs, _) = phone.link.poll(now);
    for o in outs {
        match o {
            Output::Transmit {
                leg,
                destination,
                contents,
            } => {
                let s = if leg == Leg::Up {
                    &phone.up
                } else {
                    &phone.down
                };
                let _ = s.send_to(&contents, destination);
            }
            Output::Media {
                from,
                codec,
                sealed,
                ..
            } => {
                let opened = phone.keys.open(codec, &sealed).expect("a member opens it");
                phone.heard.push((from, opened.frame));
            }
            Output::Failed(why) => panic!("a call link failed: {why}"),
            Output::Connected | Output::KeyframeWanted | Output::Lost | Output::Restored => {}
        }
    }
}

#[tokio::test]
async fn two_members_of_a_chat_call_each_other_through_the_server() {
    let (s, _dir) = start(&["alice", "bob", "carol"]).await;
    let addr = s.local_addr.to_string();
    let mut alice = Device::new("alice@localhost", Uuid::new_v4()).unwrap();
    let mut bob = Device::new("bob@localhost", Uuid::new_v4()).unwrap();
    let mut a = online(&addr, &mut alice).await;
    let mut b = online(&addr, &mut bob).await;
    let group = start_direct(&mut a, &mut alice, "bob").await;
    let (welcome, _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeInvite { welcome, .. } => Some(welcome),
        _ => None,
    })
    .await
    .expect("invitation delivered");
    bob.join(&welcome).unwrap();

    // Alice calls. Bob's app hears it ring before he picks up.
    let (mut pa, alice_id) = join(&mut a, &alice, &group).await.expect("alice joins");
    let (ring, _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::CallRoster { members, .. } => Some(members),
        _ => None,
    })
    .await
    .expect("the call rings for bob");
    assert_eq!(ring.len(), 1);
    assert_eq!(ring[0].username, "alice");
    assert_eq!(ring[0].participant, alice_id);

    let (mut pb, _) = join(&mut b, &bob, &group).await.expect("bob picks up");

    let mut rosters = Vec::new();
    let voice = |n: u32| format!("alice says {n}").into_bytes();
    let mut sent = 0u32;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && pb.heard.len() < 40 {
        signalling(&mut a, &mut pa, &group, &mut rosters).await;
        signalling(&mut b, &mut pb, &group, &mut rosters).await;
        pump(&mut pa);
        pump(&mut pb);
        if pa.link.is_connected() && pb.link.is_connected() {
            let sealed = pa.keys.seal(Codec::Opus, &voice(sent)).unwrap();
            pa.link
                .send(
                    Instant::now(),
                    Codec::Opus,
                    Duration::from_millis(20 * u64::from(sent)),
                    sealed,
                )
                .unwrap();
            sent += 1;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        pb.heard.len() >= 40,
        "bob heard {} of {sent} frames",
        pb.heard.len()
    );
    for (from, frame) in &pb.heard {
        assert_eq!(from.0, alice_id, "the server says whose voice it is");
        assert!(frame.starts_with(b"alice says "));
    }
    assert!(
        rosters.iter().any(|r| r.len() == 2),
        "both are shown in the call: {rosters:?}"
    );

    // Carol is not in the chat and cannot get into its call.
    let mut carol = Device::new("carol@localhost", Uuid::new_v4()).unwrap();
    let mut c = online(&addr, &mut carol).await;
    // `join` reads keys only after the server says yes, so Carol never needs any.
    let refused = join(&mut c, &carol, &group).await;
    assert!(matches!(refused, Err((403, _))), "{:?}", refused.err());
}

/// A device that comes online while a call is already running in one of its
/// chats is rung at once. Before, it heard of the call only when the roster
/// next changed — and a phone opened from the call's push notification connects
/// after the call started, so it never showed the incoming call at all.
#[tokio::test]
async fn a_device_that_connects_mid_call_is_rung() {
    let (s, _dir) = start(&["alice", "bob"]).await;
    let addr = s.local_addr.to_string();
    let mut alice = Device::new("alice@localhost", Uuid::new_v4()).unwrap();
    let mut bob = Device::new("bob@localhost", Uuid::new_v4()).unwrap();
    let mut a = online(&addr, &mut alice).await;
    let mut b = online(&addr, &mut bob).await;
    let group = start_direct(&mut a, &mut alice, "bob").await;
    let (welcome, _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeInvite { welcome, .. } => Some(welcome),
        _ => None,
    })
    .await
    .expect("invitation delivered");
    bob.join(&welcome).unwrap();
    // Bob's app is closed.
    drop(b);

    let (_pa, alice_id) = join(&mut a, &alice, &group).await.expect("alice joins");

    // And opened again, from the notification.
    let mut b = online(&addr, &mut bob).await;
    let (ring, _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::CallRoster {
            group: g, members, ..
        } if g == group => Some(members),
        _ => None,
    })
    .await
    .expect("the running call rings for bob as soon as he is back");
    assert_eq!(ring.len(), 1);
    assert_eq!(ring[0].username, "alice");
    assert_eq!(ring[0].participant, alice_id);
}

/// Talk from `from` to `to` for a while; returns how many frames arrived, and
/// every non-media message either side was sent meanwhile.
async fn talk(
    (fc, from): (&mut Client, &mut Phone),
    (tc, to): (&mut Client, &mut Phone),
    group: &[u8],
    for_ms: u64,
    sent: &mut u32,
) -> (usize, Vec<NetMessage>) {
    let before = to.heard.len();
    let mut said = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(for_ms);
    while Instant::now() < deadline {
        said.extend(signalling_all(fc, from, group).await);
        said.extend(signalling_all(tc, to, group).await);
        pump(from);
        pump(to);
        if from.link.is_connected() && to.link.is_connected() {
            let sealed = from.keys.seal(Codec::Opus, b"hello").unwrap();
            from.link
                .send(
                    Instant::now(),
                    Codec::Opus,
                    Duration::from_millis(20 * u64::from(*sent)),
                    sealed,
                )
                .unwrap();
            *sent += 1;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    (to.heard.len() - before, said)
}

async fn control(c: &mut Client, group: &[u8], action: cartopolis_core::CallAction) {
    send(
        c,
        &NetMessage::CallControl {
            group: group.to_vec(),
            action,
        },
    )
    .await;
}

/// A meeting's host mutes, lets talk and removes at the server (protocol 41):
/// a held member's sound stops at the forwarder, a member's attempt at a host
/// action is refused without ending the call, and a removed member is told and
/// kept out.
#[tokio::test]
async fn a_host_mutes_lets_talk_and_removes_at_the_forwarder() {
    use cartopolis_core::{CallAction, CallEndReason, CallRole, CALL_CONTROL_REQUEST};
    let (s, _dir) = start(&["alice", "bob"]).await;
    let addr = s.local_addr.to_string();
    let mut alice = Device::new("alice@localhost", Uuid::new_v4()).unwrap();
    let mut bob = Device::new("bob@localhost", Uuid::new_v4()).unwrap();
    let mut a = online(&addr, &mut alice).await;
    let mut b = online(&addr, &mut bob).await;
    let group = start_direct(&mut a, &mut alice, "bob").await;
    let (welcome, _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeInvite { welcome, .. } => Some(welcome),
        _ => None,
    })
    .await
    .expect("invitation delivered");
    bob.join(&welcome).unwrap();

    let (mut pa, _) = join(&mut a, &alice, &group).await.expect("alice joins");
    let (mut pb, bob_id) = join(&mut b, &bob, &group).await.expect("bob joins");
    let mut sent = 0;

    // Bob is heard, and the first one in hosts the call.
    let (heard, said) = talk(
        (&mut b, &mut pb),
        (&mut a, &mut pa),
        &group,
        3000,
        &mut sent,
    )
    .await;
    assert!(heard > 20, "alice heard {heard} of bob's frames");
    let roster = said
        .iter()
        .rev()
        .find_map(|m| match m {
            NetMessage::CallRoster { members, .. } if members.len() == 2 => Some(members.clone()),
            _ => None,
        })
        .expect("a roster with both");
    let host = roster.iter().find(|m| m.role == CallRole::Host).unwrap();
    assert_eq!(host.username, "alice");

    // Bob is no host: his try is refused, and he is still in the call.
    control(&mut b, &group, CallAction::MuteAll).await;
    let (_, said) = talk((&mut b, &mut pb), (&mut a, &mut pa), &group, 500, &mut sent).await;
    assert!(said.iter().any(|m| matches!(
        m,
        NetMessage::ServerError { request_id, code: 403, .. } if *request_id == CALL_CONTROL_REQUEST
    )));

    // Alice mutes him: the roster says so, and his sound stops at the server.
    control(
        &mut a,
        &group,
        CallAction::Mute {
            participant: bob_id,
        },
    )
    .await;
    let (_, said) = talk((&mut b, &mut pb), (&mut a, &mut pa), &group, 500, &mut sent).await;
    assert!(said.iter().any(|m| matches!(
        m,
        NetMessage::CallRoster { members, .. }
            if members.iter().any(|m| m.participant == bob_id && m.held)
    )));
    let (heard, _) = talk(
        (&mut b, &mut pb),
        (&mut a, &mut pa),
        &group,
        1500,
        &mut sent,
    )
    .await;
    assert_eq!(heard, 0, "a held member is not forwarded");

    // Let him talk again.
    control(
        &mut a,
        &group,
        CallAction::AllowTalk {
            participant: bob_id,
        },
    )
    .await;
    let _ = talk((&mut b, &mut pb), (&mut a, &mut pa), &group, 300, &mut sent).await;
    let (heard, _) = talk(
        (&mut b, &mut pb),
        (&mut a, &mut pa),
        &group,
        1500,
        &mut sent,
    )
    .await;
    assert!(heard > 20, "alice heard {heard} after letting bob talk");

    // Remove him: he is told, and cannot come back while the call lasts.
    control(
        &mut a,
        &group,
        CallAction::Remove {
            participant: bob_id,
        },
    )
    .await;
    let (_, said) = talk((&mut b, &mut pb), (&mut a, &mut pa), &group, 500, &mut sent).await;
    assert!(said.iter().any(|m| matches!(
        m,
        NetMessage::CallEnded { reason: CallEndReason::Removed { by }, .. } if by == "alice"
    )));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let back = join(&mut b, &bob, &group).await;
    assert!(matches!(back, Err((409, _))), "{:?}", back.err());
}
