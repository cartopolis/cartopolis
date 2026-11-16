//! People on other servers: contact requests and private chats between
//! servers, each person connected only to their own
//! (`docs/notes/chat-across-servers.md`; stage 5 of
//! `docs/mockups/federation/README.md`, its first part).
//!
//! Two ideas carry all of it:
//!
//! - **Somebody elsewhere is an account here too**, made the first time they
//!   are named (`accounts.home`, V45): their username is their full address,
//!   they cannot sign in, and every rule keyed by account (contacts, requests,
//!   blocks, chat membership, devices) holds for them as it is. A request or an
//!   answer is applied on both servers, each to its own copy, so each enforces
//!   the rules for its own people.
//! - **A chat lives on the server its id names** (its hub, `address::group_home`).
//!   The hub orders its log as it always has; a member's home forwards what
//!   their devices send to the hub (`submit`), the hub forwards what it
//!   appends to every member's home (`deliver`, `invite`), and a device that
//!   catches up asks its home, which asks the hub (`sync`). A device never
//!   connects anywhere but home.
//!
//! Servers call each other with signed JSON POSTs under [`PREFIX`], signed by
//! the instance actor the fediverse already gives every server, so a server is
//! believed to speak for the people whose addresses end in its name and for
//! nobody else. Nothing here reads a sealed byte: the hub relays what it
//! relayed before, and the follower relays it on.

pub mod routes;

use std::collections::HashMap;

use bytes::Bytes;
use cartopolis_core::{address, encode_app_frame, NetMessage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::SimConfig;
use crate::db::SharedConn;
use crate::fediverse::{fetch::Client, instance_url, key_id, signing, Federation};

/// Where server-to-server calls go: under `/ap/`, which a deployment already
/// sends to the simulator for the fediverse, so no proxy needs a new route.
pub const PREFIX: &str = "/ap/chat";

/// What a username made by this server can never hold, so a full address is
/// never also the name of somebody here.
pub const ADDRESS_MARK: char = '@';

/// How this server reaches others for chat: who it is, and the client and key
/// the fediverse already has.
#[derive(Clone)]
pub struct Link {
    /// Whether it talks to other servers at all: the fediverse's switch, so a
    /// development sim on `localhost` keeps to itself.
    pub enabled: bool,
    /// This server's name (`cartopolis.org`).
    pub server: String,
    base: String,
    client: Client,
    test_bases: HashMap<String, String>,
}

impl Link {
    #[must_use]
    pub fn from_config(config: &SimConfig) -> Self {
        let fed = Federation::from_config(config);
        Self {
            enabled: fed.enabled,
            server: fed.server,
            base: fed.base,
            client: fed.client,
            test_bases: if config.fediverse_allow_private {
                config.peer_test_bases.clone()
            } else {
                HashMap::new()
            },
        }
    }

    /// The public origin of `server`.
    #[must_use]
    pub fn base_of(&self, server: &str) -> String {
        if server.eq_ignore_ascii_case(&self.server) {
            return self.base.clone();
        }
        self.test_bases
            .get(&server.to_lowercase())
            .cloned()
            .unwrap_or_else(|| format!("https://{server}"))
    }

    /// The only key `server` may sign these calls with: its instance actor's.
    #[must_use]
    pub fn key_id_of(&self, server: &str) -> String {
        key_id(&instance_url(&self.base_of(server)))
    }

    /// The name and server of `who` when it is a full address on another
    /// server; `None` for somebody here, or a bare name.
    #[must_use]
    pub fn elsewhere<'a>(&self, who: &'a str) -> Option<(&'a str, &'a str)> {
        address::split(who.trim()).filter(|(_, s)| !s.eq_ignore_ascii_case(&self.server))
    }

    /// Whether `group` is a chat another server is the hub of, and which.
    #[must_use]
    pub fn hub_elsewhere(&self, group: &[u8]) -> Option<String> {
        address::group_home(group)
            .filter(|h| !h.eq_ignore_ascii_case(&self.server))
            .map(str::to_lowercase)
    }

    /// POST `body` to `server`'s `path`, signed, and read its answer. Blocking:
    /// call from a blocking thread ([`post`] does). A refusal is the words the
    /// other server gave, for the person to read.
    fn call<T: DeserializeOwned>(
        &self,
        conn: &SharedConn,
        server: &str,
        path: &str,
        body: &[u8],
    ) -> Result<T, String> {
        if !self.enabled {
            return Err(OFF.into());
        }
        let key = {
            let c = conn.lock().map_err(|_| "the database is unavailable")?;
            signing::private_key(&c, "instance", crate::fediverse::now_ms())
                .map_err(|e| format!("no key to sign with: {e}"))?
        };
        let url = format!("{}{PREFIX}{path}", self.base_of(server));
        match self
            .client
            .post_signed_json(&url, body, &key, &key_id(&instance_url(&self.base)))
        {
            Ok((200, bytes)) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("{server} answered something unreadable: {e}")),
            Ok((status, bytes)) if (400..500).contains(&status) && status != 404 => {
                let why = String::from_utf8_lossy(&bytes).trim().to_string();
                Err(if why.is_empty() {
                    format!("{server} refused ({status})")
                } else {
                    why
                })
            }
            Ok((status, _)) => {
                tracing::info!(%url, status, "chat elsewhere: refused");
                Err(format!("{server} does not take chats from other servers"))
            }
            Err(e) => {
                tracing::info!(%url, "chat elsewhere: {e}");
                Err(format!("{server} could not be reached"))
            }
        }
    }
}

/// What a person reads when this server does not talk to others.
pub const OFF: &str = "this server does not talk to other servers";

/// [`Link::call`] on a blocking thread.
pub async fn post<B: Serialize, T: DeserializeOwned + Send + 'static>(
    link: &Link,
    conn: &SharedConn,
    server: &str,
    path: &'static str,
    body: &B,
) -> Result<T, String> {
    let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let (link, conn, server) = (link.clone(), conn.clone(), server.to_string());
    tokio::task::spawn_blocking(move || link.call(&conn, &server, path, &bytes))
        .await
        .map_err(|e| e.to_string())?
}

// ── people elsewhere, as accounts here ──────────────────────────────────────

/// The account here that stands for `who`, a full address on another server
/// than `here`, made the first time they are named. `None` for somebody here
/// or a bare name, and when an account made here already holds the address
/// as its name (from before names could not hold an `@`).
pub fn account(conn: &Connection, who: &str, here: &str) -> rusqlite::Result<Option<String>> {
    let Some((name, server)) = address::split(who.trim()) else {
        return Ok(None);
    };
    if server.eq_ignore_ascii_case(here) {
        return Ok(None);
    }
    let username = address::person(name, server);
    let server = server.to_lowercase();
    // Anyone may send them a request *here*: whether they take it is their
    // home's rule, applied there (`routes::contact_request`).
    conn.execute(
        "INSERT OR IGNORE INTO accounts (id, username, pw_hash, home, request_policy)
         VALUES (?1, ?2, '!elsewhere', ?3, 0)",
        params![Uuid::new_v4().to_string(), username, server],
    )?;
    conn.query_row(
        "SELECT id FROM accounts WHERE username = ?1 AND home = ?2",
        params![username, server],
        |r| r.get(0),
    )
    .optional()
}

/// The server an account's person lives on, when it is not this one.
pub fn home_of(conn: &Connection, account: &str) -> rusqlite::Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT home FROM accounts WHERE id = ?1",
            params![account],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Record `device` as `account`'s, the way a device registering does, for a
/// person elsewhere whose device this server learnt of. Whether it is theirs:
/// a device id already somebody else's is not.
pub fn own_device(conn: &Connection, account: &str, device: Uuid) -> rusqlite::Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO e2ee_devices (device, account) VALUES (?1, ?2)",
        params![device.to_string(), account],
    )?;
    let owner: Option<String> = conn
        .query_row(
            "SELECT account FROM e2ee_devices WHERE device = ?1",
            params![device.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(owner.as_deref() == Some(account))
}

// ── what servers send each other ────────────────────────────────────────────

/// Bytes as base64 in JSON.
mod b64 {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&B64.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        B64.decode(s).map_err(serde::de::Error::custom)
    }
}

/// `from` (elsewhere) asks `to` (a name on the receiving server) to be
/// contacts.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContactRequest {
    pub from: String,
    pub to: String,
    pub link_code: Option<String>,
}

/// What the request came to: `true` when `to` had asked first, and the two
/// are contacts now.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContactRequested {
    pub contacts: bool,
}

/// `from` (elsewhere) answers the request `to` (here) sent them.
#[derive(Debug, Serialize, Deserialize)]
pub struct ContactAnswer {
    pub from: String,
    pub to: String,
    pub accept: bool,
}

/// `by` (elsewhere) looks up `name` here, for the person card.
#[derive(Debug, Serialize, Deserialize)]
pub struct LookUp {
    pub by: String,
    pub name: String,
}

/// What the card shows, as this server sees the two; `None` for nobody, or
/// for somebody who takes requests only through their link.
#[derive(Debug, Serialize, Deserialize)]
pub struct Person {
    pub person: Option<cartopolis_core::PersonDto>,
}

/// An empty answer.
#[derive(Debug, Serialize, Deserialize)]
pub struct Done {}

/// `by` (elsewhere) claims one key package of each of `of`'s devices.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClaimKeys {
    pub by: String,
    pub of: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KeyPackage {
    pub device: Uuid,
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Keys {
    pub packages: Vec<KeyPackage>,
}

/// An invitation sent with a commit, its members as full addresses.
#[derive(Debug, Serialize, Deserialize)]
pub struct Welcome {
    pub devices: Vec<Uuid>,
    pub members: Vec<String>,
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
}

/// An `E2eeSend` from `from`'s `device`, forwarded by their home to the hub.
#[derive(Debug, Serialize, Deserialize)]
pub struct Submit {
    pub from: String,
    pub device: Uuid,
    #[serde(with = "b64")]
    pub group: Vec<u8>,
    pub epoch: u64,
    pub commit: bool,
    #[serde(with = "b64")]
    pub message: Vec<u8>,
    pub welcome: Option<Welcome>,
}

/// The hub's answer: the entry's place in the log, or why not.
#[derive(Debug, Serialize, Deserialize)]
pub struct Submitted {
    pub seq: Option<u64>,
    pub refused: Option<String>,
}

/// One entry of a chat's log, its sender as a full address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    #[serde(with = "b64")]
    pub group: Vec<u8>,
    pub seq: u64,
    pub sender: String,
    pub device: Uuid,
    pub sent_at_ms: u64,
    pub commit: bool,
    #[serde(with = "b64")]
    pub message: Vec<u8>,
    /// The moderator who deleted it, as a full address.
    pub redacted_by: Option<String>,
}

impl Entry {
    /// A log entry as the hub (`home`) sends it on.
    #[must_use]
    pub fn of(e: &crate::e2ee::Entry, home: &str) -> Self {
        Self {
            group: e.group.clone(),
            seq: e.seq,
            sender: address::qualify(&e.sender_name, home),
            device: e.device,
            sent_at_ms: e.sent_at_ms,
            commit: e.is_commit,
            message: e.message.clone(),
            redacted_by: e
                .redacted_by
                .as_deref()
                .map(|by| address::qualify(by, home)),
        }
    }

    /// The entry as the wire delivers it to a device.
    #[must_use]
    pub fn message(self) -> NetMessage {
        match self.redacted_by {
            Some(by) => NetMessage::E2eeRedacted {
                group: self.group,
                seq: self.seq,
                by,
            },
            None => NetMessage::E2eeDeliver {
                group: self.group,
                seq: self.seq,
                sender: self.sender,
                device: self.device,
                sent_at_ms: self.sent_at_ms,
                commit: self.commit,
                message: self.message,
            },
        }
    }
}

/// An entry the hub appended, for `to` (names on the receiving server).
#[derive(Debug, Serialize, Deserialize)]
pub struct Deliver {
    pub to: Vec<String>,
    pub entry: Entry,
}

/// An invitation for `to`'s `device`, kept by their home until it joins.
#[derive(Debug, Serialize, Deserialize)]
pub struct Invite {
    pub to: String,
    pub device: Uuid,
    #[serde(with = "b64")]
    pub group: Vec<u8>,
    pub from: String,
    pub after: u64,
    #[serde(with = "b64")]
    pub welcome: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Cursor {
    #[serde(with = "b64")]
    pub group: Vec<u8>,
    pub after: u64,
}

/// `who`'s `device` catching up on chats the receiving server is the hub of.
#[derive(Debug, Serialize, Deserialize)]
pub struct Sync {
    pub who: String,
    pub device: Uuid,
    pub cursors: Vec<Cursor>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Synced {
    pub entries: Vec<Entry>,
}

// ── the hub's side: what it appended goes to every member's home ────────────

/// After the hub appends `entry`, send it on to the homes of the members who
/// live elsewhere, and each invitation to its device's home. In the
/// background: a slow server delays nobody here, and a device that misses a
/// delivery catches up by `sync`, which is how gaps are filled anyway.
pub fn forward_appended(
    link: &Link,
    conn: &SharedConn,
    members: &[String],
    invites: &[crate::e2ee::Invite],
    entry: &crate::e2ee::Entry,
) {
    if !link.enabled {
        return;
    }
    let (link, conn) = (link.clone(), conn.clone());
    let members = members.to_vec();
    let invites = invites.to_vec();
    let entry = Entry::of(entry, &link.server);
    tokio::spawn(async move {
        let found = crate::db::call(&conn, move |c| -> rusqlite::Result<_> {
            let mut homes: HashMap<String, Vec<String>> = HashMap::new();
            for m in &members {
                let row: Option<(String, Option<String>)> = c
                    .query_row(
                        "SELECT username, home FROM accounts WHERE id = ?1",
                        params![m],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((username, Some(home))) = row {
                    if let Some((name, _)) = address::split(&username) {
                        homes.entry(home).or_default().push(name.to_string());
                    }
                }
            }
            let mut sent_invites = Vec::new();
            for inv in invites {
                let row: Option<(String, Option<String>)> = c
                    .query_row(
                        "SELECT a.username, a.home FROM e2ee_devices d
                         JOIN accounts a ON a.id = d.account WHERE d.device = ?1",
                        params![inv.device.to_string()],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((username, Some(home))) = row {
                    if let Some((name, _)) = address::split(&username) {
                        sent_invites.push((home, name.to_string(), inv));
                    }
                }
            }
            Ok((homes, sent_invites))
        })
        .await;
        let Ok(Ok((homes, invites))) = found else {
            return;
        };
        // Invitations first: a device joins at its invitation's `after`, and
        // reads on from there.
        for (home, to, inv) in invites {
            let body = Invite {
                to,
                device: inv.device,
                group: inv.group,
                from: address::qualify(&inv.from, &link.server),
                after: inv.after,
                welcome: inv.welcome,
            };
            if let Err(e) = post::<_, Done>(&link, &conn, &home, "/invite", &body).await {
                tracing::info!(%home, "chat elsewhere: invitation not delivered: {e}");
            }
        }
        for (home, to) in homes {
            let body = Deliver {
                to,
                entry: entry.clone(),
            };
            if let Err(e) = post::<_, Done>(&link, &conn, &home, "/deliver", &body).await {
                tracing::info!(%home, "chat elsewhere: entry not delivered: {e}");
            }
        }
    });
}

/// Hand a frame to every connected device of the named accounts here, minus
/// those who blocked `sender` (for anything but a commit).
pub fn deliver_here(
    chat: &crate::chat::ChatHub,
    accounts: &[String],
    msg: &NetMessage,
    sender: &str,
    commit: bool,
) {
    if let Ok(frame) = encode_app_frame(msg) {
        chat.deliver_private(accounts, sender, commit, &Bytes::from(frame));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn somebody_elsewhere_is_one_account_here_however_they_are_typed() {
        let conn = crate::db::open_and_migrate(":memory:").unwrap();
        let a = account(&conn, "Bob@Rug.Example", "cartopolis.org")
            .unwrap()
            .unwrap();
        let b = account(&conn, "bob@rug.example", "cartopolis.org")
            .unwrap()
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(home_of(&conn, &a).unwrap().as_deref(), Some("rug.example"));
        // Nobody here, and nobody bare, is made into one.
        assert_eq!(
            account(&conn, "bob@cartopolis.org", "cartopolis.org").unwrap(),
            None
        );
        assert_eq!(account(&conn, "bob", "cartopolis.org").unwrap(), None);
        // And they cannot sign in: the password is no PHC string.
        let hash: String = conn
            .query_row("SELECT pw_hash FROM accounts WHERE id = ?1", [&a], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!crate::auth::verify_password("", &hash));
    }

    #[test]
    fn a_device_is_theirs_only_if_nobody_had_it() {
        let conn = crate::db::open_and_migrate(":memory:").unwrap();
        let bob = account(&conn, "bob@rug.example", "here").unwrap().unwrap();
        let eve = account(&conn, "eve@evil.example", "here").unwrap().unwrap();
        let d = Uuid::new_v4();
        assert!(own_device(&conn, &bob, d).unwrap());
        assert!(own_device(&conn, &bob, d).unwrap());
        assert!(!own_device(&conn, &eve, d).unwrap());
    }

    #[test]
    fn a_server_signs_with_its_own_key_only() {
        let config = SimConfig {
            public_base_url: "https://a.example".into(),
            ..SimConfig::default()
        };
        let link = Link::from_config(&config);
        assert_eq!(
            link.key_id_of("a.example"),
            "https://a.example/ap/actor#main-key"
        );
        assert_eq!(
            link.key_id_of("b.example"),
            "https://b.example/ap/actor#main-key"
        );
        assert_eq!(link.elsewhere("bob@b.example"), Some(("bob", "b.example")));
        assert_eq!(link.elsewhere("bob@A.example"), None);
        assert_eq!(link.elsewhere("bob"), None);
        assert_eq!(
            link.hub_elsewhere(&address::group_id("b.example", [0; 16]))
                .as_deref(),
            Some("b.example")
        );
        assert_eq!(
            link.hub_elsewhere(&address::group_id("a.example", [0; 16])),
            None
        );
    }
}
