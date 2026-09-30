//! Contacts and message requests (protocol 27, `docs/notes/contacts.md`).
//!
//! A contact is somebody you accepted, both ways round. Anybody else reaches
//! you by a request, and the rules about requests are enforced here, on the
//! server, because a rule in the client is a rule a modified client skips:
//!
//! 1. **Who may send you one** is your [`RequestPolicy`]: anyone, people who
//!    were near your avatar in the last hour or share a contact with you (the
//!    default), or only people holding your contact link. The link works
//!    whatever the policy says.
//! 2. **How many anyone may send.** [`NEW_ACCOUNT_DAILY`] a day for an account
//!    younger than [`NEW_ACCOUNT_DAYS`], [`DAILY`] for everybody else.
//! 3. **A decline is silent.** The sender goes on seeing their request as
//!    waiting, and cannot send the same person another one for
//!    [`RESEND_AFTER_DAYS`]. A request to somebody who has blocked you is
//!    recorded as declined on arrival, which is the same silence.
//! 4. **A private chat with somebody who is not a contact starts as a
//!    request.** Starting one needs a request between the two ([`may_invite`]),
//!    and while it is unanswered the sender may write one message in it
//!    ([`may_send`]). Declining takes the decliner out of the chat's delivery
//!    list, so nothing more reaches them — without a commit, which the sender
//!    would see.
//!
//! Plain functions over a `&Connection`, as `e2ee` is, so the tests run the
//! rules against an in-memory database.

use cartopolis_core::{
    ContactDto, ContactRequestDto, ContactSettings, MetDto, NetMessage, PersonDto, Relation,
    RequestPolicy,
};
use rusqlite::{params, Connection, OptionalExtension};

/// An account younger than this is new: its requests carry the mark, and it
/// may send fewer of them.
pub const NEW_ACCOUNT_DAYS: i64 = 7;
/// Requests a new account may send in a day.
pub const NEW_ACCOUNT_DAILY: i64 = 5;
/// Requests an established account may send in a day.
pub const DAILY: i64 = 30;
/// A declined request stops the same person asking again for this long.
pub const RESEND_AFTER_DAYS: i64 = 30;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

const WAITING: i64 = 0;
const ACCEPTED: i64 = 1;
const DECLINED: i64 = 2;
const WITHDRAWN: i64 = 3;

/// Why a request was refused, in words the client shows.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refusal {
    #[error("there is nobody called {0}")]
    NoSuchPerson(String),
    #[error("that is you")]
    Yourself,
    #[error("{0} only takes requests from people they have been near, or who share a contact")]
    OnlyNear(String),
    #[error("{0} only takes requests through their contact link")]
    OnlyLink(String),
    #[error("that is as many requests as you can send today; try again tomorrow")]
    TooMany,
    #[error("{0} is not one of your contacts yet: send them a request first")]
    NotAContact(String),
    #[error("waiting for {0} to accept your request")]
    Waiting(String),
    #[error("there is no request from {0}")]
    NoRequest(String),
    #[error("the server could not store that: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for Refusal {
    fn from(e: rusqlite::Error) -> Self {
        Refusal::Storage(e.to_string())
    }
}

/// One account, as the rules need it.
#[derive(Debug, Clone)]
pub struct Account {
    pub id: String,
    pub username: String,
    pub created_ms: i64,
    policy: RequestPolicy,
    show_online: bool,
    notify_requests: bool,
    separate_new_accounts: bool,
    link_code: Option<String>,
}

impl Account {
    #[must_use]
    pub fn is_new(&self, now_ms: u64) -> bool {
        (now_ms as i64 - self.created_ms) < NEW_ACCOUNT_DAYS * DAY_MS
    }
}

const ACCOUNT_COLUMNS: &str = "id, username, CAST(strftime('%s', created_at) AS INTEGER) * 1000, \
     request_policy, show_online, notify_requests, separate_new_accounts, link_code";

fn account_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        username: r.get(1)?,
        created_ms: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
        policy: RequestPolicy::from_db(r.get(3)?),
        show_online: r.get(4)?,
        notify_requests: r.get(5)?,
        separate_new_accounts: r.get(6)?,
        link_code: r.get(7)?,
    })
}

/// An enabled account by username, ignoring case: people type names.
pub fn account_by_name(conn: &Connection, username: &str) -> Result<Option<Account>, Refusal> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {ACCOUNT_COLUMNS} FROM accounts \
                 WHERE username = ?1 COLLATE NOCASE AND disabled = 0"
            ),
            params![username.trim()],
            account_row,
        )
        .optional()?)
}

/// An enabled account by id.
pub fn account_by_id(conn: &Connection, id: &str) -> Result<Option<Account>, Refusal> {
    Ok(conn
        .query_row(
            &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1 AND disabled = 0"),
            params![id],
            account_row,
        )
        .optional()?)
}

fn new_link_code() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// This account's settings, making its link code on first use.
pub fn settings(conn: &Connection, me: &Account) -> Result<ContactSettings, Refusal> {
    let link_code = match &me.link_code {
        Some(c) => c.clone(),
        None => {
            let c = new_link_code();
            conn.execute(
                "UPDATE accounts SET link_code = ?2 WHERE id = ?1",
                params![me.id, c],
            )?;
            c
        }
    };
    Ok(ContactSettings {
        policy: me.policy,
        show_online: me.show_online,
        notify_requests: me.notify_requests,
        separate_new_accounts: me.separate_new_accounts,
        link_code,
    })
}

/// Change who may reach this account.
#[allow(clippy::too_many_arguments)]
pub fn set_settings(
    conn: &Connection,
    me: &str,
    policy: RequestPolicy,
    show_online: bool,
    notify_requests: bool,
    separate_new_accounts: bool,
    reset_link: bool,
) -> Result<(), Refusal> {
    conn.execute(
        "UPDATE accounts SET request_policy = ?2, show_online = ?3, notify_requests = ?4,
                separate_new_accounts = ?5 WHERE id = ?1",
        params![
            me,
            policy.to_db(),
            show_online,
            notify_requests,
            separate_new_accounts
        ],
    )?;
    if reset_link {
        conn.execute(
            "UPDATE accounts SET link_code = ?2 WHERE id = ?1",
            params![me, new_link_code()],
        )?;
    }
    Ok(())
}

/// Whether two accounts are contacts.
pub fn are_contacts(conn: &Connection, a: &str, b: &str) -> Result<bool, Refusal> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM contacts WHERE account = ?1 AND contact = ?2",
            params![a, b],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// An account's contacts, by id.
pub fn contact_ids(conn: &Connection, me: &str) -> Result<Vec<String>, Refusal> {
    let mut stmt = conn.prepare("SELECT contact FROM contacts WHERE account = ?1")?;
    let rows = stmt.query_map(params![me], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The usernames of the contacts two accounts both have.
pub fn in_common(conn: &Connection, a: &str, b: &str) -> Result<Vec<String>, Refusal> {
    let mut stmt = conn.prepare(
        "SELECT acc.username FROM contacts x
         JOIN contacts y ON y.contact = x.contact AND y.account = ?2
         JOIN accounts acc ON acc.id = x.contact
         WHERE x.account = ?1 AND acc.disabled = 0
         ORDER BY acc.username COLLATE NOCASE",
    )?;
    let rows = stmt.query_map(params![a, b], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn blocks(conn: &Connection, blocker: &str, username: &str) -> Result<bool, Refusal> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM blocks WHERE blocker_id = ?1 AND blocked_username = ?2 COLLATE NOCASE",
            params![blocker, username],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The latest request from one account to another: its id, status and when
/// it was answered.
fn latest_request(
    conn: &Connection,
    from: &str,
    to: &str,
) -> Result<Option<(i64, i64, Option<i64>)>, Refusal> {
    Ok(conn
        .query_row(
            "SELECT id, status, answered_ms FROM contact_requests
             WHERE from_acc = ?1 AND to_acc = ?2 ORDER BY id DESC LIMIT 1",
            params![from, to],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
}

fn waiting_request(conn: &Connection, from: &str, to: &str) -> Result<Option<i64>, Refusal> {
    Ok(latest_request(conn, from, to)?
        .filter(|(_, status, _)| *status == WAITING)
        .map(|(id, _, _)| id))
}

/// How `me` stands with `them`.
pub fn relation(conn: &Connection, me: &str, them: &str) -> Result<Relation, Refusal> {
    if me == them {
        return Ok(Relation::Me);
    }
    if are_contacts(conn, me, them)? {
        return Ok(Relation::Contact);
    }
    if waiting_request(conn, them, me)?.is_some() {
        return Ok(Relation::RequestReceived);
    }
    // A declined request still reads as sent: the decline is silent.
    if latest_request(conn, me, them)?.is_some_and(|(_, s, _)| s == WAITING || s == DECLINED) {
        return Ok(Relation::RequestSent);
    }
    Ok(Relation::Nobody)
}

/// Which of `names` `me` may see on a chat's World view (`WherePeople`): their
/// contacts who show that they are online, as their own spelling of the name.
///
/// Where somebody's avatar stands is the same kind of fact as whether they are
/// online, so it rides the same switch: hiding one and showing the other would
/// be a setting that says less than it seems to. Anyone else is dropped rather
/// than marked, so the answer says nothing about whether they exist.
pub fn visible_to(conn: &Connection, me: &str, names: &[String]) -> Result<Vec<String>, Refusal> {
    let mut out = Vec::new();
    for name in names {
        let Some(them) = account_by_name(conn, name)? else {
            continue;
        };
        if them.show_online
            && them.id != me
            && are_contacts(conn, me, &them.id)?
            && !out.contains(&them.username)
        {
            out.push(them.username);
        }
    }
    Ok(out)
}

/// Whether `target`'s setting lets `me` send them a request, before the daily
/// limit. `met` is whether the two were near each other in the last hour.
pub fn policy_allows(
    conn: &Connection,
    me: &Account,
    target: &Account,
    met: bool,
    link_code: Option<&str>,
) -> Result<Result<(), Refusal>, Refusal> {
    if link_code.is_some_and(|c| target.link_code.as_deref() == Some(c)) {
        return Ok(Ok(()));
    }
    Ok(match target.policy {
        RequestPolicy::Anyone => Ok(()),
        RequestPolicy::Near => {
            if met || !in_common(conn, &me.id, &target.id)?.is_empty() {
                Ok(())
            } else {
                Err(Refusal::OnlyNear(target.username.clone()))
            }
        }
        RequestPolicy::LinkOnly => Err(Refusal::OnlyLink(target.username.clone())),
    })
}

fn sent_today(conn: &Connection, me: &str, now_ms: u64) -> Result<i64, Refusal> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM contact_requests WHERE from_acc = ?1 AND sent_at_ms > ?2",
        params![me, now_ms as i64 - DAY_MS],
        |r| r.get(0),
    )?)
}

fn make_contacts(
    conn: &Connection,
    a: &str,
    b: &str,
    met: Option<MetDto>,
    now_ms: u64,
) -> Result<(), Refusal> {
    for (x, y) in [(a, b), (b, a)] {
        conn.execute(
            "INSERT OR IGNORE INTO contacts (account, contact, since_ms, met_at_ms, met_lat, met_lng)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                x,
                y,
                now_ms as i64,
                met.map(|m| m.at_ms as i64),
                met.map(|m| m.latitude),
                met.map(|m| m.longitude)
            ],
        )?;
    }
    Ok(())
}

/// What a request came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    /// It is waiting for the other person (or, silently, will wait forever).
    Waiting,
    /// They had already asked you, so you are contacts now.
    Contacts,
    /// Nothing changed: you were already contacts, or already asked.
    Unchanged,
}

/// Send a request from `me` to `target`, applying rules 1–3.
pub fn request(
    conn: &Connection,
    me: &Account,
    target: &Account,
    met: Option<MetDto>,
    link_code: Option<&str>,
    now_ms: u64,
) -> Result<Sent, Refusal> {
    if me.id == target.id {
        return Err(Refusal::Yourself);
    }
    if are_contacts(conn, &me.id, &target.id)? {
        return Ok(Sent::Unchanged);
    }
    // They asked first: asking back is saying yes.
    if let Some(id) = waiting_request(conn, &target.id, &me.id)? {
        conn.execute(
            "UPDATE contact_requests SET status = ?2, answered_ms = ?3 WHERE id = ?1",
            params![id, ACCEPTED, now_ms as i64],
        )?;
        make_contacts(conn, &me.id, &target.id, met, now_ms)?;
        return Ok(Sent::Contacts);
    }
    if let Some((_, status, answered)) = latest_request(conn, &me.id, &target.id)? {
        if status == WAITING {
            return Ok(Sent::Unchanged);
        }
        let recent = answered.is_some_and(|a| now_ms as i64 - a < RESEND_AFTER_DAYS * DAY_MS);
        if status == DECLINED && recent {
            return Ok(Sent::Unchanged);
        }
    }
    policy_allows(conn, me, target, met.is_some(), link_code)??;
    let limit = if me.is_new(now_ms) {
        NEW_ACCOUNT_DAILY
    } else {
        DAILY
    };
    if sent_today(conn, &me.id, now_ms)? >= limit {
        return Err(Refusal::TooMany);
    }
    // Blocked: stored as declined on arrival, which the sender cannot tell
    // from waiting (rule 3).
    let status = if blocks(conn, &target.id, &me.username)? {
        DECLINED
    } else {
        WAITING
    };
    conn.execute(
        "INSERT INTO contact_requests (from_acc, to_acc, sent_at_ms, status, answered_ms,
                                       met_at_ms, met_lat, met_lng)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            me.id,
            target.id,
            now_ms as i64,
            status,
            (status == DECLINED).then_some(now_ms as i64),
            met.map(|m| m.at_ms as i64),
            met.map(|m| m.latitude),
            met.map(|m| m.longitude)
        ],
    )?;
    if status == DECLINED {
        leave_direct_chat(conn, &target.id, &me.id)?;
    }
    Ok(Sent::Waiting)
}

/// Take `leaver` out of the delivery list of their direct chat with `other`,
/// and drop the invitations still waiting for their devices. No commit is
/// written, so `other` sees no change: that is what keeps a decline silent.
pub fn leave_direct_chat(conn: &Connection, leaver: &str, other: &str) -> Result<(), Refusal> {
    let (a, b) = if leaver < other {
        (leaver, other)
    } else {
        (other, leaver)
    };
    let group: Option<Vec<u8>> = conn
        .query_row(
            "SELECT id FROM e2ee_groups WHERE direct_a = ?1 AND direct_b = ?2",
            params![a, b],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(g) = group {
        conn.execute(
            "DELETE FROM e2ee_members WHERE group_id = ?1 AND account = ?2",
            params![g, leaver],
        )?;
        conn.execute(
            "DELETE FROM e2ee_invites WHERE group_id = ?1
               AND device IN (SELECT device FROM e2ee_devices WHERE account = ?2)",
            params![g, leaver],
        )?;
    }
    Ok(())
}

/// Accept or decline the request `from` sent `me`.
pub fn answer(
    conn: &Connection,
    me: &Account,
    from: &Account,
    accept: bool,
    now_ms: u64,
) -> Result<(), Refusal> {
    let Some(id) = waiting_request(conn, &from.id, &me.id)? else {
        return Err(Refusal::NoRequest(from.username.clone()));
    };
    let met: Option<MetDto> = conn.query_row(
        "SELECT met_at_ms, met_lat, met_lng FROM contact_requests WHERE id = ?1",
        params![id],
        |r| {
            Ok(match (r.get::<_, Option<i64>>(0)?, r.get(1)?, r.get(2)?) {
                (Some(at), Some(latitude), Some(longitude)) => Some(MetDto {
                    at_ms: at as u64,
                    latitude,
                    longitude,
                }),
                _ => None,
            })
        },
    )?;
    conn.execute(
        "UPDATE contact_requests SET status = ?2, answered_ms = ?3 WHERE id = ?1",
        params![id, if accept { ACCEPTED } else { DECLINED }, now_ms as i64],
    )?;
    if accept {
        make_contacts(conn, &me.id, &from.id, met, now_ms)?;
    } else {
        leave_direct_chat(conn, &me.id, &from.id)?;
    }
    Ok(())
}

/// Stop being contacts with `them`, or withdraw a request sent to them. Not a
/// block, and the chat between you stays.
pub fn remove(conn: &Connection, me: &str, them: &str) -> Result<(), Refusal> {
    conn.execute(
        "DELETE FROM contacts WHERE (account = ?1 AND contact = ?2) OR (account = ?2 AND contact = ?1)",
        params![me, them],
    )?;
    // Withdrawing a waiting request frees the person to be asked again; a
    // declined one stays declined, or withdrawing would be a way round the
    // resend wait.
    conn.execute(
        "UPDATE contact_requests SET status = ?3 WHERE from_acc = ?1 AND to_acc = ?2 AND status = ?4",
        params![me, them, WITHDRAWN, WAITING],
    )?;
    Ok(())
}

/// Whether `me` may add `target`'s devices to a private chat (rule 4): they
/// are contacts, or there is a request between them either way round.
///
/// A *declined* request from `me` counts too, or the refusal would tell `me`
/// what the decline must not: the chat is made as usual, and [`silenced_by`]
/// then keeps everything in it from reaching the person who declined.
pub fn may_invite(conn: &Connection, me: &str, target: &str) -> Result<bool, Refusal> {
    Ok(me == target
        || are_contacts(conn, me, target)?
        || latest_request(conn, me, target)?.is_some_and(|(_, s, _)| s == WAITING || s == DECLINED)
        || waiting_request(conn, target, me)?.is_some())
}

/// The other person in `group`, if it is a direct chat whose other person
/// declined `me`'s request and is not a contact: what `me` writes there must
/// not reach them, and nothing may say so to `me` (rule 3).
pub fn silenced_by(conn: &Connection, me: &str, group: &[u8]) -> Result<Option<String>, Refusal> {
    let pair: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT direct_a, direct_b FROM e2ee_groups WHERE id = ?1",
            params![group],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((Some(a), Some(b))) = pair else {
        return Ok(None);
    };
    let other = if a == me { b } else { a };
    if are_contacts(conn, me, &other)? {
        return Ok(None);
    }
    let declined = latest_request(conn, me, &other)?.is_some_and(|(_, s, _)| s == DECLINED);
    Ok(declined.then_some(other))
}

/// Whether `me` may write an application message into `group` (rule 4). Only a
/// direct chat with somebody who is not a contact is limited, and only while a
/// request from `me` to them is unanswered (or declined, which looks the same):
/// then one message. A direct chat from before contacts existed has no request
/// behind it and is left alone.
pub fn may_send(conn: &Connection, me: &str, group: &[u8]) -> Result<(), Refusal> {
    let pair: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT direct_a, direct_b FROM e2ee_groups WHERE id = ?1",
            params![group],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((Some(a), Some(b))) = pair else {
        return Ok(());
    };
    let other = if a == me { b } else { a };
    if are_contacts(conn, me, &other)? {
        return Ok(());
    }
    let pending =
        latest_request(conn, me, &other)?.is_some_and(|(_, s, _)| s == WAITING || s == DECLINED);
    if !pending {
        return Ok(());
    }
    let written: i64 = conn.query_row(
        "SELECT COUNT(*) FROM e2ee_log WHERE group_id = ?1 AND sender = ?2 AND is_commit = 0",
        params![group, me],
        |r| r.get(0),
    )?;
    if written >= 1 {
        let name = account_by_id(conn, &other)?
            .map(|a| a.username)
            .unwrap_or_default();
        return Err(Refusal::Waiting(name));
    }
    Ok(())
}

fn met_of(at: Option<i64>, lat: Option<f64>, lng: Option<f64>) -> Option<MetDto> {
    Some(MetDto {
        at_ms: at? as u64,
        latitude: lat?,
        longitude: lng?,
    })
}

/// Everything about `me`'s contacts, for [`NetMessage::ContactsState`].
/// `online` says whether an account has a connection now.
pub fn state(
    conn: &Connection,
    me: &Account,
    online: &dyn Fn(&str) -> bool,
    now_ms: u64,
) -> Result<NetMessage, Refusal> {
    let mut contacts = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT acc.id, acc.username, acc.show_online, c.since_ms, c.met_at_ms, c.met_lat, c.met_lng
             FROM contacts c JOIN accounts acc ON acc.id = c.contact
             WHERE c.account = ?1 AND acc.disabled = 0
             ORDER BY acc.username COLLATE NOCASE",
        )?;
        let rows = stmt.query_map(params![me.id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, i64>(3)?,
                met_of(r.get(4)?, r.get(5)?, r.get(6)?),
            ))
        })?;
        for row in rows {
            let (id, username, shows, since, met) = row?;
            contacts.push(ContactDto {
                username,
                since_ms: since as u64,
                met,
                online: shows.then(|| online(&id)),
            });
        }
    }
    let requests = |incoming: bool| -> Result<Vec<ContactRequestDto>, Refusal> {
        let sql = if incoming {
            "SELECT acc.id, acc.username, CAST(strftime('%s', acc.created_at) AS INTEGER) * 1000,
                    r.sent_at_ms, r.met_at_ms, r.met_lat, r.met_lng
             FROM contact_requests r JOIN accounts acc ON acc.id = r.from_acc
             WHERE r.to_acc = ?1 AND r.status = 0 AND acc.disabled = 0
             ORDER BY r.sent_at_ms DESC"
        } else {
            "SELECT acc.id, acc.username, CAST(strftime('%s', acc.created_at) AS INTEGER) * 1000,
                    r.sent_at_ms, r.met_at_ms, r.met_lat, r.met_lng
             FROM contact_requests r JOIN accounts acc ON acc.id = r.to_acc
             WHERE r.from_acc = ?1 AND r.status IN (0, 2) AND acc.disabled = 0
               AND r.id = (SELECT MAX(id) FROM contact_requests
                           WHERE from_acc = r.from_acc AND to_acc = r.to_acc)
             ORDER BY r.sent_at_ms DESC"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![me.id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, i64>(3)?,
                met_of(r.get(4)?, r.get(5)?, r.get(6)?),
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, username, joined, sent, met) = row?;
            // Already contacts (they asked back, or were contacts before): not a
            // request any more, whatever the old row says.
            if are_contacts(conn, &me.id, &id)? {
                continue;
            }
            out.push(ContactRequestDto {
                username,
                sent_at_ms: sent as u64,
                met,
                new_account: (now_ms as i64 - joined) < NEW_ACCOUNT_DAYS * DAY_MS,
                joined_ms: joined.max(0) as u64,
                in_common: in_common(conn, &me.id, &id)?,
            });
        }
        Ok(out)
    };
    Ok(NetMessage::ContactsState {
        contacts,
        incoming: requests(true)?,
        outgoing: requests(false)?,
        settings: settings(conn, me)?,
    })
}

/// The person card: what `me` may know about `them`. `None` when `them` asked
/// not to be found by name and has nothing to do with `me` (rule 1's
/// link-only setting), unless they are standing near `me` right now — then
/// the card still shows, since the avatar is in plain sight, just with no way
/// to send a request.
pub fn person(
    conn: &Connection,
    me: &Account,
    them: &Account,
    met: Option<MetDto>,
    distance_m: Option<f32>,
    now_ms: u64,
) -> Result<Option<PersonDto>, Refusal> {
    let rel = relation(conn, &me.id, &them.id)?;
    if them.policy == RequestPolicy::LinkOnly && rel == Relation::Nobody && distance_m.is_none() {
        return Ok(None);
    }
    let may_request = rel == Relation::Nobody
        && policy_allows(conn, me, them, met.is_some(), None)?.is_ok()
        && !latest_request(conn, &me.id, &them.id)?.is_some_and(|(_, s, _)| s == DECLINED);
    Ok(Some(PersonDto {
        username: them.username.clone(),
        joined_ms: them.created_ms.max(0) as u64,
        new_account: them.is_new(now_ms),
        in_common: in_common(conn, &me.id, &them.id)?,
        relation: rel,
        met,
        distance_m,
        may_request,
    }))
}

/// Forget an account's contacts and requests, both ways round.
pub fn erase_account(conn: &Connection, id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM contacts WHERE account = ?1 OR contact = ?1",
        params![id],
    )?;
    conn.execute(
        "DELETE FROM contact_requests WHERE from_acc = ?1 OR to_acc = ?1",
        params![id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn db() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut c).unwrap();
        c
    }

    fn account(c: &Connection, name: &str, age_days: i64) -> Account {
        let id = uuid::Uuid::new_v4().to_string();
        let secs = NOW as i64 / 1000 - age_days * 86_400;
        c.execute(
            "INSERT INTO accounts (id, username, pw_hash, created_at)
             VALUES (?1, ?2, 'x', datetime(?3, 'unixepoch'))",
            params![id, name, secs],
        )
        .unwrap();
        account_by_id(c, &id).unwrap().unwrap()
    }

    fn reload(c: &Connection, a: &Account) -> Account {
        account_by_id(c, &a.id).unwrap().unwrap()
    }

    fn policy(c: &Connection, a: &Account, p: RequestPolicy) -> Account {
        set_settings(c, &a.id, p, true, false, true, false).unwrap();
        reload(c, a)
    }

    const MET: MetDto = MetDto {
        at_ms: NOW,
        latitude: 53.2,
        longitude: 6.5,
    };

    #[test]
    fn the_default_takes_requests_only_from_people_you_have_met() {
        let c = db();
        let (a, b) = (account(&c, "anna", 100), account(&c, "bas", 100));
        assert_eq!(b.policy, RequestPolicy::Near);
        assert_eq!(
            request(&c, &a, &b, None, None, NOW),
            Err(Refusal::OnlyNear("bas".into()))
        );
        assert_eq!(request(&c, &a, &b, Some(MET), None, NOW), Ok(Sent::Waiting));
        assert_eq!(relation(&c, &a.id, &b.id).unwrap(), Relation::RequestSent);
        assert_eq!(
            relation(&c, &b.id, &a.id).unwrap(),
            Relation::RequestReceived
        );
    }

    #[test]
    fn a_shared_contact_counts_as_having_met() {
        let c = db();
        let (a, b, t) = (
            account(&c, "anna", 100),
            account(&c, "bas", 100),
            account(&c, "tom", 100),
        );
        make_contacts(&c, &a.id, &t.id, None, NOW).unwrap();
        make_contacts(&c, &b.id, &t.id, None, NOW).unwrap();
        assert_eq!(request(&c, &a, &b, None, None, NOW), Ok(Sent::Waiting));
        assert_eq!(
            in_common(&c, &a.id, &b.id).unwrap(),
            vec!["tom".to_string()]
        );
    }

    #[test]
    fn the_link_works_whatever_the_setting() {
        let c = db();
        let a = account(&c, "anna", 100);
        let b = account(&c, "bas", 100);
        let b = policy(&c, &b, RequestPolicy::LinkOnly);
        let code = settings(&c, &b).unwrap().link_code;
        let b = reload(&c, &b);
        assert_eq!(
            request(&c, &a, &b, Some(MET), None, NOW),
            Err(Refusal::OnlyLink("bas".into()))
        );
        assert_eq!(
            request(&c, &a, &b, None, Some("wrong"), NOW),
            Err(Refusal::OnlyLink("bas".into()))
        );
        assert_eq!(
            request(&c, &a, &b, None, Some(&code), NOW),
            Ok(Sent::Waiting)
        );
    }

    #[test]
    fn a_reset_link_stops_the_old_one() {
        let c = db();
        let (a, b) = (account(&c, "anna", 100), account(&c, "bas", 100));
        let old = settings(&c, &b).unwrap().link_code;
        set_settings(&c, &b.id, RequestPolicy::LinkOnly, true, false, true, true).unwrap();
        let b = reload(&c, &b);
        assert_ne!(b.link_code.as_deref(), Some(old.as_str()));
        assert!(request(&c, &a, &b, None, Some(&old), NOW).is_err());
    }

    #[test]
    fn nobody_is_hidden_from_a_name_search_unless_they_ask_to_be() {
        let c = db();
        let (a, b) = (account(&c, "anna", 100), account(&c, "bas", 100));
        assert!(person(&c, &a, &b, None, None, NOW).unwrap().is_some());
        let b = policy(&c, &b, RequestPolicy::LinkOnly);
        assert!(person(&c, &a, &b, None, None, NOW).unwrap().is_none());
        // Standing next to you they are in plain sight, but not reachable.
        let card = person(&c, &a, &b, Some(MET), Some(10.0), NOW)
            .unwrap()
            .unwrap();
        assert!(!card.may_request);
    }

    #[test]
    fn a_new_account_may_send_fewer_requests_a_day() {
        let c = db();
        let newbie = account(&c, "new", 1);
        for i in 0..NEW_ACCOUNT_DAILY {
            let t = policy(
                &c,
                &account(&c, &format!("t{i}"), 100),
                RequestPolicy::Anyone,
            );
            assert_eq!(request(&c, &newbie, &t, None, None, NOW), Ok(Sent::Waiting));
        }
        let t = policy(&c, &account(&c, "one_more", 100), RequestPolicy::Anyone);
        assert_eq!(
            request(&c, &newbie, &t, None, None, NOW),
            Err(Refusal::TooMany)
        );
        // A day later it may again.
        assert_eq!(
            request(&c, &newbie, &t, None, None, NOW + DAY_MS as u64 + 1),
            Ok(Sent::Waiting)
        );
    }

    #[test]
    fn accepting_makes_contacts_both_ways_and_asking_back_is_accepting() {
        let c = db();
        let (a, b) = (account(&c, "anna", 100), account(&c, "bas", 100));
        let b = policy(&c, &b, RequestPolicy::Anyone);
        request(&c, &a, &b, Some(MET), None, NOW).unwrap();
        answer(&c, &b, &a, true, NOW).unwrap();
        assert!(are_contacts(&c, &a.id, &b.id).unwrap());
        assert!(are_contacts(&c, &b.id, &a.id).unwrap());

        let (x, y) = (account(&c, "xan", 100), account(&c, "yor", 100));
        let (x, y) = (
            policy(&c, &x, RequestPolicy::Anyone),
            policy(&c, &y, RequestPolicy::Anyone),
        );
        request(&c, &x, &y, None, None, NOW).unwrap();
        assert_eq!(request(&c, &y, &x, None, None, NOW), Ok(Sent::Contacts));
        assert!(are_contacts(&c, &x.id, &y.id).unwrap());
    }

    #[test]
    fn a_decline_looks_like_waiting_and_stops_a_resend() {
        let c = db();
        let (a, b) = (account(&c, "anna", 100), account(&c, "bas", 100));
        let b = policy(&c, &b, RequestPolicy::Anyone);
        request(&c, &a, &b, None, None, NOW).unwrap();
        answer(&c, &b, &a, false, NOW).unwrap();
        assert_eq!(relation(&c, &a.id, &b.id).unwrap(), Relation::RequestSent);
        assert_eq!(relation(&c, &b.id, &a.id).unwrap(), Relation::Nobody);
        let NetMessage::ContactsState { outgoing, .. } = state(&c, &a, &|_| false, NOW).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(outgoing.len(), 1, "the sender still sees it waiting");
        let NetMessage::ContactsState { incoming, .. } = state(&c, &b, &|_| false, NOW).unwrap()
        else {
            unreachable!()
        };
        assert!(incoming.is_empty());
        // Asking again changes nothing, and withdrawing does not reset it.
        assert_eq!(request(&c, &a, &b, None, None, NOW), Ok(Sent::Unchanged));
        remove(&c, &a.id, &b.id).unwrap();
        assert_eq!(request(&c, &a, &b, None, None, NOW), Ok(Sent::Unchanged));
        assert_eq!(relation(&c, &b.id, &a.id).unwrap(), Relation::Nobody);
    }

    #[test]
    fn a_request_to_somebody_who_blocked_you_is_silently_declined() {
        let c = db();
        let (a, b) = (account(&c, "anna", 100), account(&c, "bas", 100));
        let b = policy(&c, &b, RequestPolicy::Anyone);
        crate::db::set_block(&c, &b.id, "anna", true).unwrap();
        assert_eq!(request(&c, &a, &b, None, None, NOW), Ok(Sent::Waiting));
        assert_eq!(relation(&c, &a.id, &b.id).unwrap(), Relation::RequestSent);
        assert_eq!(relation(&c, &b.id, &a.id).unwrap(), Relation::Nobody);
    }

    #[test]
    fn a_private_chat_needs_a_request_and_holds_one_message() {
        let c = db();
        let (a, b) = (account(&c, "anna", 100), account(&c, "bas", 100));
        let b = policy(&c, &b, RequestPolicy::Anyone);
        assert!(!may_invite(&c, &a.id, &b.id).unwrap());
        request(&c, &a, &b, None, None, NOW).unwrap();
        assert!(may_invite(&c, &a.id, &b.id).unwrap());

        let group = b"direct-ab".to_vec();
        crate::e2ee::create_group(&c, &a.id, &group, Some("bas")).unwrap();
        let log = |sender: &str, seq: i64, commit: bool| {
            c.execute(
                "INSERT INTO e2ee_log (group_id, seq, sender, sender_name, device, is_commit, sent_at_ms, message)
                 VALUES (?1, ?2, ?3, 'n', 'd', ?4, 0, x'00')",
                params![group, seq, sender, commit],
            )
            .unwrap();
        };
        log(&a.id, 1, true);
        assert_eq!(may_send(&c, &a.id, &group), Ok(()));
        log(&a.id, 2, false);
        assert_eq!(
            may_send(&c, &a.id, &group),
            Err(Refusal::Waiting("bas".into()))
        );
        answer(&c, &b, &a, true, NOW).unwrap();
        assert_eq!(may_send(&c, &a.id, &group), Ok(()));
    }

    #[test]
    fn declining_takes_you_out_of_the_chat_without_telling_them() {
        let c = db();
        let (a, b) = (account(&c, "anna", 100), account(&c, "bas", 100));
        let b = policy(&c, &b, RequestPolicy::Anyone);
        request(&c, &a, &b, None, None, NOW).unwrap();
        let group = b"direct-ab".to_vec();
        crate::e2ee::create_group(&c, &a.id, &group, Some("bas")).unwrap();
        c.execute(
            "INSERT INTO e2ee_members (group_id, account) VALUES (?1, ?2)",
            params![group, b.id],
        )
        .unwrap();
        answer(&c, &b, &a, false, NOW).unwrap();
        let members = crate::e2ee::members(&c, &group).unwrap();
        assert_eq!(members, vec![a.id.clone()]);
        let head: i64 = c
            .query_row(
                "SELECT head FROM e2ee_groups WHERE id = ?1",
                params![group],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(head, 0, "no entry was written that the sender could see");
    }

    #[test]
    fn a_chat_from_before_contacts_is_left_alone() {
        let c = db();
        let (a, b) = (account(&c, "anna", 100), account(&c, "bas", 100));
        let group = b"old".to_vec();
        crate::e2ee::create_group(&c, &a.id, &group, Some("bas")).unwrap();
        c.execute(
            "INSERT INTO e2ee_log (group_id, seq, sender, sender_name, device, is_commit, sent_at_ms, message)
             VALUES (?1, 1, ?2, 'n', 'd', 0, 0, x'00')",
            params![group, a.id],
        )
        .unwrap();
        assert_eq!(may_send(&c, &a.id, &group), Ok(()));
        let _ = b;
    }

    #[test]
    fn deleting_an_account_forgets_its_contacts() {
        let c = db();
        let (a, b) = (account(&c, "anna", 100), account(&c, "bas", 100));
        make_contacts(&c, &a.id, &b.id, None, NOW).unwrap();
        erase_account(&c, &a.id).unwrap();
        assert!(contact_ids(&c, &b.id).unwrap().is_empty());
    }

    #[test]
    fn only_contacts_who_show_online_are_placed_on_a_map() {
        let c = db();
        let (a, b, _sanne, h) = (
            account(&c, "anna", 100),
            account(&c, "bas", 100),
            account(&c, "sanne", 100),
            account(&c, "hidden", 100),
        );
        make_contacts(&c, &a.id, &b.id, None, NOW).unwrap();
        make_contacts(&c, &a.id, &h.id, None, NOW).unwrap();
        set_settings(&c, &h.id, RequestPolicy::Near, false, true, true, false).unwrap();
        let ask: Vec<String> = ["BAS", "bas", "sanne", "hidden", "anna", "nobody"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        // Sanne is no contact, Hidden hides being online, Anna is the asker,
        // and a name asked twice (in any case) is answered once.
        assert_eq!(
            visible_to(&c, &a.id, &ask).unwrap(),
            vec!["bas".to_string()]
        );
    }
}
