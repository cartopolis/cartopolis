//! Two people on two servers become contacts and talk privately, each
//! connected only to their own server (`docs/notes/chat-across-servers.md`).
//! Both servers are real and sign and verify every call between them; the
//! devices are real MLS devices (`cartopolis_e2ee`). Only
//! `fediverse_allow_private` and `peer_test_bases` let two servers on one
//! machine reach each other over plain `http`.

use std::collections::HashMap;
use std::sync::Arc;

use cartopolis_core::{NetMessage, RequestPolicy};
use cartopolis_e2ee::Device;
use cartopolis_simulator::{ensure_account, serve, SimConfig, SimHandle};
use uuid::Uuid;

use super::private_chat::{next_matching, online, say, send, start_direct, text, Client, PATIENT};

struct Server {
    _handle: SimHandle,
    _dir: tempfile::TempDir,
    addr: String,
    base: String,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A server called `host` with `user` on it, that finds `peer` at `peer_base`,
/// with a call forwarder when `calls`.
async fn server(
    host: &str,
    port: u16,
    user: &str,
    peer: &str,
    peer_base: &str,
    calls: bool,
) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("world.db").to_str().unwrap().to_string();
    {
        let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
        ensure_account(&conn, user, "pw").unwrap();
    }
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
        peer_test_bases: HashMap::from([(peer.to_string(), peer_base.to_string())]),
        calls_listen: calls.then(|| "127.0.0.1:0".to_string()),
        ..SimConfig::default()
    };
    let handle = serve(Arc::new(config)).await.unwrap();
    Server {
        addr: handle.local_addr.to_string(),
        _handle: handle,
        _dir: dir,
        base,
    }
}

/// Alice's server is `localhost`, Bob's `127.0.0.1`: two names for one
/// machine, as two servers' names are two hosts.
async fn two_servers() -> (Server, Server) {
    servers(false).await
}

/// [`two_servers`], Alice's with a call forwarder when `calls`; Bob's never has one.
async fn servers(calls: bool) -> (Server, Server) {
    let (pa, pb) = (free_port(), free_port());
    let a = server(
        "localhost",
        pa,
        "alice",
        "127.0.0.1",
        &format!("http://127.0.0.1:{pb}"),
        calls,
    )
    .await;
    let b = server(
        "127.0.0.1",
        pb,
        "bob",
        "localhost",
        &format!("http://localhost:{pa}"),
        false,
    )
    .await;
    (a, b)
}

/// The next contact state pushed to `c`.
async fn contacts_state(c: &mut Client) -> (Vec<String>, Vec<String>, Vec<String>) {
    next_matching(c, PATIENT, |m| match m {
        NetMessage::ContactsState {
            contacts,
            incoming,
            outgoing,
            ..
        } => Some((
            contacts.into_iter().map(|c| c.username).collect(),
            incoming.into_iter().map(|r| r.username).collect(),
            outgoing.into_iter().map(|r| r.username).collect(),
        )),
        _ => None,
    })
    .await
    .expect("contact state")
    .0
}

/// Keep reading contact states until one satisfies `f`.
async fn contacts_until(
    c: &mut Client,
    f: impl Fn(&(Vec<String>, Vec<String>, Vec<String>)) -> bool,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    for _ in 0..10 {
        let s = contacts_state(c).await;
        if f(&s) {
            return s;
        }
    }
    panic!("the contact state never came round");
}

#[tokio::test]
async fn people_on_two_servers_become_contacts_and_talk_privately() {
    let (sa, sb) = two_servers().await;
    let mut alice = Device::new("alice@localhost", Uuid::new_v4()).unwrap();
    let mut bob = Device::new("bob@127.0.0.1", Uuid::new_v4()).unwrap();
    let mut a = online(&sa.addr, &mut alice).await;
    let mut b = online(&sb.addr, &mut bob).await;

    // Bob takes requests only from people he has been near (the default),
    // and his own server says so, in its words, through Alice's.
    send(
        &mut a,
        &NetMessage::RequestContact {
            request_id: 20,
            username: "bob@127.0.0.1".into(),
            link_code: None,
        },
    )
    .await;
    let (refusal, _) = next_matching(&mut a, PATIENT, |m| match m {
        NetMessage::ServerError {
            request_id: 20,
            message,
            ..
        } => Some(message),
        _ => None,
    })
    .await
    .expect("refused");
    assert!(refusal.contains("only takes requests"), "{refusal}");

    // With no request between them, his keys are not hers to take.
    send(
        &mut a,
        &NetMessage::E2eeClaimKeyPackages {
            request_id: 25,
            address: "bob@127.0.0.1".into(),
        },
    )
    .await;
    let (refusal, _) = next_matching(&mut a, PATIENT, |m| match m {
        NetMessage::ServerError {
            request_id: 25,
            message,
            ..
        } => Some(message),
        _ => None,
    })
    .await
    .expect("refused");
    assert!(refusal.contains("not one of your contacts"), "{refusal}");

    // Bob opens up; Alice asks again, and the request reaches him at home.
    send(
        &mut b,
        &NetMessage::SetContactSettings {
            request_id: 21,
            policy: RequestPolicy::Anyone,
            show_online: true,
            notify_requests: false,
            separate_new_accounts: false,
            reset_link: false,
        },
    )
    .await;
    contacts_state(&mut b).await;
    // Her card for him comes from his server, with the button on it.
    send(
        &mut a,
        &NetMessage::LookupPerson {
            request_id: 24,
            username: Some("bob@127.0.0.1".into()),
            avatar_id: None,
        },
    )
    .await;
    let (card, _) = next_matching(&mut a, PATIENT, |m| match m {
        NetMessage::PersonInfo { person, .. } => Some(person),
        _ => None,
    })
    .await
    .expect("looked up");
    let card = card.expect("somebody there");
    assert_eq!(card.username, "bob@127.0.0.1");
    assert!(card.may_request);
    send(
        &mut a,
        &NetMessage::RequestContact {
            request_id: 22,
            username: "bob@127.0.0.1".into(),
            link_code: None,
        },
    )
    .await;
    let (_, _, outgoing) = contacts_until(&mut a, |s| !s.2.is_empty()).await;
    assert_eq!(outgoing, ["bob@127.0.0.1"]);
    let (_, incoming, _) = contacts_until(&mut b, |s| !s.1.is_empty()).await;
    assert_eq!(incoming, ["alice@localhost"]);

    // He accepts, and both servers know them as contacts.
    send(
        &mut b,
        &NetMessage::AnswerContact {
            request_id: 23,
            username: "alice@localhost".into(),
            accept: true,
        },
    )
    .await;
    let (contacts, _, _) = contacts_until(&mut b, |s| !s.0.is_empty()).await;
    assert_eq!(contacts, ["alice@localhost"]);
    let (contacts, _, _) = contacts_until(&mut a, |s| !s.0.is_empty()).await;
    assert_eq!(contacts, ["bob@127.0.0.1"]);

    // Alice starts a private chat: his key package comes from his server, the
    // chat lives on hers, and the invitation goes to his.
    let group = start_direct(&mut a, &mut alice, "bob@127.0.0.1").await;
    let ((from, welcome), _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeInvite { from, welcome, .. } => Some((from, welcome)),
        _ => None,
    })
    .await
    .expect("invitation delivered through his server");
    assert_eq!(from, "alice@localhost");
    assert_eq!(bob.join(&welcome).unwrap(), group);

    let secret = "the key is under the third flowerpot";
    let seq = say(&mut a, &mut alice, &group, secret).await;
    let ((message, sender), raw) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeDeliver {
            message,
            sender,
            seq: s,
            ..
        } if s == seq => Some((message, sender)),
        _ => None,
    })
    .await
    .expect("message delivered through his server");
    assert_eq!(sender, "alice@localhost");
    assert!(
        !raw.windows(secret.len()).any(|w| w == secret.as_bytes()),
        "nothing either server relays holds the words"
    );
    assert_eq!(text(&mut bob, &group, &message), secret);

    // He answers from his own server; it is ordered by hers.
    let seq = say(&mut b, &mut bob, &group, "found it").await;
    assert_eq!(seq, 3, "the hub's log: the commit, her line, his");
    let ((message, sender), _) = next_matching(&mut a, PATIENT, |m| match m {
        NetMessage::E2eeDeliver {
            message,
            sender,
            seq: s,
            ..
        } if s == seq => Some((message, sender)),
        _ => None,
    })
    .await
    .expect("his answer reached her");
    assert_eq!(sender, "bob@127.0.0.1");
    assert_eq!(text(&mut alice, &group, &message), "found it");

    // Away, and back: what was said meanwhile comes from her server, asked
    // for by his.
    drop(b);
    let seq = say(&mut a, &mut alice, &group, "see you at the market").await;
    let mut b = online(&sb.addr, &mut bob).await;
    send(
        &mut b,
        &NetMessage::E2eeSync {
            request_id: 30,
            cursors: vec![cartopolis_core::E2eeCursor {
                group: group.clone(),
                after: 3,
            }],
        },
    )
    .await;
    let (message, _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeDeliver {
            message, seq: s, ..
        } if s == seq => Some(message),
        _ => None,
    })
    .await
    .expect("caught up through his server");
    assert_eq!(text(&mut bob, &group, &message), "see you at the market");
}

#[tokio::test]
async fn a_server_is_believed_only_with_its_own_signature() {
    let (sa, _sb) = two_servers().await;
    // Unsigned: refused before anything is read.
    let url = format!("{}/ap/chat/deliver", sa.base);
    let status = tokio::task::spawn_blocking(move || {
        match ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_string("{}")
        {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(e) => panic!("{e}"),
        }
    })
    .await
    .unwrap();
    assert_eq!(status, 401);
}

/// Alice (on `localhost`) and Bob (on `127.0.0.1`) as contacts with a direct
/// chat, which lives on her server.
async fn a_chat_across(sa: &Server, sb: &Server) -> (Device, Device, Client, Client, Vec<u8>) {
    let mut alice = Device::new("alice@localhost", Uuid::new_v4()).unwrap();
    let mut bob = Device::new("bob@127.0.0.1", Uuid::new_v4()).unwrap();
    let mut a = online(&sa.addr, &mut alice).await;
    let mut b = online(&sb.addr, &mut bob).await;
    send(
        &mut b,
        &NetMessage::SetContactSettings {
            request_id: 21,
            policy: RequestPolicy::Anyone,
            show_online: true,
            notify_requests: false,
            separate_new_accounts: false,
            reset_link: false,
        },
    )
    .await;
    contacts_state(&mut b).await;
    send(
        &mut a,
        &NetMessage::RequestContact {
            request_id: 22,
            username: "bob@127.0.0.1".into(),
            link_code: None,
        },
    )
    .await;
    contacts_until(&mut b, |s| !s.1.is_empty()).await;
    send(
        &mut b,
        &NetMessage::AnswerContact {
            request_id: 23,
            username: "alice@localhost".into(),
            accept: true,
        },
    )
    .await;
    contacts_until(&mut a, |s| !s.0.is_empty()).await;
    let group = start_direct(&mut a, &mut alice, "bob@127.0.0.1").await;
    let (welcome, _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::E2eeInvite { welcome, .. } => Some(welcome),
        _ => None,
    })
    .await
    .expect("invitation delivered through his server");
    bob.join(&welcome).unwrap();
    (alice, bob, a, b, group)
}

/// The names in the last roster among `said` for `group`.
fn last_roster(said: &[NetMessage], group: &[u8]) -> Option<Vec<String>> {
    said.iter().rev().find_map(|m| match m {
        NetMessage::CallRoster {
            group: g, members, ..
        } if g == group => Some(members.iter().map(|m| m.username.clone()).collect()),
        _ => None,
    })
}

/// A call in a chat across two servers runs on the forwarder of the chat's
/// hub: Bob's server has no forwarder at all, rings him for Alice's call,
/// forwards his join and answers to hers, and his media goes straight to hers
/// — sealed with the chat's keys, as in a call on one server.
#[tokio::test]
async fn people_on_two_servers_call_each_other_on_the_chats_hub() {
    use super::calls::{join, talk_until};
    let (sa, sb) = servers(true).await;
    let (alice, bob, mut a, mut b, group) = a_chat_across(&sa, &sb).await;

    // Alice calls; his phone rings, through his server, with her full address.
    let (mut pa, alice_id) = join(&mut a, &alice, &group).await.expect("alice calls");
    let (ring, _) = next_matching(&mut b, PATIENT, |m| match m {
        NetMessage::CallRoster {
            group: g, members, ..
        } if g == group => Some(members),
        _ => None,
    })
    .await
    .expect("the call rings for bob at his server");
    assert_eq!(ring.len(), 1);
    assert_eq!(ring[0].username, "alice@localhost");
    assert_eq!(ring[0].participant, alice_id);

    // He picks up at his own server, which has no forwarder: hers answers.
    let (mut pb, bob_id) = join(&mut b, &bob, &group)
        .await
        .expect("bob joins through his server");
    assert_ne!(bob_id, alice_id);

    // Her voice reaches him, and his her, through her forwarder.
    let mut sent = 0;
    let (heard, said) = talk_until(
        (&mut a, &mut pa),
        (&mut b, &mut pb),
        &group,
        15_000,
        &mut sent,
        |n, _| n >= 20,
    )
    .await;
    assert!(heard >= 20, "bob heard {heard} of {sent} frames from alice");
    let (heard, said_back) = talk_until(
        (&mut b, &mut pb),
        (&mut a, &mut pa),
        &group,
        15_000,
        &mut sent,
        |n, _| n >= 20,
    )
    .await;
    assert!(heard >= 20, "alice heard {heard} frames from bob");
    // Each sees both, named as their own server names people.
    let rosters: Vec<NetMessage> = said.into_iter().chain(said_back).collect();
    let mut names = last_roster(&rosters, &group).expect("a roster with both");
    names.sort();
    assert!(
        names == ["alice", "bob@127.0.0.1"] || names == ["alice@localhost", "bob"],
        "{names:?}"
    );

    // He hangs up; she is alone in it.
    send(
        &mut b,
        &NetMessage::CallLeave {
            group: group.clone(),
        },
    )
    .await;
    let (alone, _) = next_matching(&mut a, PATIENT, |m| match m {
        NetMessage::CallRoster {
            group: g, members, ..
        } if g == group && members.len() == 1 => Some(members),
        _ => None,
    })
    .await
    .expect("his leaving reaches her server");
    assert_eq!(alone[0].username, "alice");
}
