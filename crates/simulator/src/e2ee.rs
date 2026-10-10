//! The delivery service for private chats (protocol 25,
//! `docs/notes/encrypted-chat.md`).
//!
//! Every byte this module stores is sealed with keys that only the members'
//! devices hold; this crate does not even link the code that could open them
//! (`cartopolis_e2ee` is a client dependency). Its whole job is four rules:
//!
//! 1. **A key package is handed out once.** Claiming one deletes it, because
//!    its private half opens exactly one invitation. A device keeps a stock and
//!    tops it up on every connect ([`register_device`] says how many are left).
//! 2. **Commits are ordered by epoch.** A membership change is accepted only if
//!    it was made in the chat's current epoch, and bumps it. The second of two
//!    simultaneous changes is refused, and its sender retries after catching
//!    up. This is what keeps every device's view of a chat identical, and it is
//!    the only thing the server has to get right for that.
//! 3. **Only members write, and only members read.** Membership here is a
//!    routing list, set by the commits that add people; cryptographic membership
//!    is the MLS state on the devices, which the server cannot forge.
//! 4. **Blocks are silent and never break a chat.** A blocked sender's
//!    *messages* are withheld from the person blocking them, as the rest of
//!    chat does; their *commits* are not, because a device that misses a commit
//!    can read nothing in that chat ever again.
//!
//! Plain functions over a `&Connection`, the way `db` is, so the tests below run
//! the rules against an in-memory database with no network.

use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

/// Largest sealed entry accepted. A text message is a few hundred bytes after
/// sealing; this leaves room for the points to come (a place and a camera pose)
/// without letting the log become a file store. Pictures go as attachments.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Largest key package accepted. An Ed25519/X25519 package is ~300 bytes.
pub const MAX_KEY_PACKAGE_BYTES: usize = 4 * 1024;

/// Key packages a device may hold. Enough for a burst of new chats between
/// connects; bounded so one device cannot fill the table.
pub const MAX_KEY_PACKAGES: u32 = 100;

/// Longest group id accepted: a server name (at most 253 bytes, the DNS limit),
/// `/`, and 16 random bytes (`cartopolis_core::address::group_id`).
pub const MAX_GROUP_ID_BYTES: usize = 270;

/// Entries replayed per chat per sync. A device further behind than this syncs
/// again from where the replay stopped.
pub const SYNC_BATCH: i64 = 500;

/// Why a request was refused, in words the client can show.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refusal {
    #[error("that device is registered to another account")]
    DeviceTaken,
    #[error("no such person")]
    NoSuchPerson,
    #[error("that chat already exists")]
    GroupExists,
    #[error("no such chat")]
    NoSuchGroup,
    #[error("you are not in that chat")]
    NotAMember,
    #[error("somebody else changed the chat first; catch up and try again")]
    StaleEpoch,
    #[error("a direct chat is between two people")]
    DirectIsTwo,
    #[error("an invitation names a device that is not theirs")]
    ForeignDevice,
    #[error("too large")]
    TooLarge,
    #[error("the server could not store that: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for Refusal {
    fn from(e: rusqlite::Error) -> Self {
        Refusal::Storage(e.to_string())
    }
}

/// One entry of a chat's log, as delivered.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub group: Vec<u8>,
    pub seq: u64,
    pub sender: String,
    pub sender_name: String,
    pub device: Uuid,
    pub is_commit: bool,
    pub sent_at_ms: u64,
    pub message: Vec<u8>,
}

/// An invitation waiting for a device.
#[derive(Debug, Clone, PartialEq)]
pub struct Invite {
    pub device: Uuid,
    pub group: Vec<u8>,
    pub from: String,
    pub after: u64,
    pub welcome: Vec<u8>,
}

/// What appending an entry produced, for the caller to deliver live.
#[derive(Debug)]
pub struct Appended {
    pub entry: Entry,
    /// Accounts to deliver the entry to (every member, the sender included:
    /// their other devices need it, and their own device skips it).
    pub members: Vec<String>,
    /// Invitations created by this entry, to push to their devices now.
    pub invites: Vec<Invite>,
}

/// Register a device for an account, store its new key packages, and say how
/// many it now has.
pub fn register_device(
    conn: &Connection,
    account: &str,
    device: Uuid,
    packages: &[Vec<u8>],
) -> Result<u32, Refusal> {
    let dev = device.to_string();
    let owner: Option<String> = conn
        .query_row(
            "SELECT account FROM e2ee_devices WHERE device = ?1",
            params![dev],
            |r| r.get(0),
        )
        .optional()?;
    match owner {
        Some(o) if o != account => return Err(Refusal::DeviceTaken),
        Some(_) => {
            conn.execute(
                "UPDATE e2ee_devices SET last_seen = datetime('now') WHERE device = ?1",
                params![dev],
            )?;
        }
        None => {
            conn.execute(
                "INSERT INTO e2ee_devices (device, account, last_seen) VALUES (?1, ?2, datetime('now'))",
                params![dev, account],
            )?;
        }
    }
    let mut have = package_count(conn, &dev)?;
    for p in packages {
        if have >= MAX_KEY_PACKAGES {
            break;
        }
        if p.is_empty() || p.len() > MAX_KEY_PACKAGE_BYTES {
            return Err(Refusal::TooLarge);
        }
        conn.execute(
            "INSERT INTO e2ee_key_packages (device, bytes) VALUES (?1, ?2)",
            params![dev, p],
        )?;
        have += 1;
    }
    Ok(have)
}

fn package_count(conn: &Connection, device: &str) -> Result<u32, Refusal> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM e2ee_key_packages WHERE device = ?1",
        params![device],
        |r| r.get(0),
    )?)
}

/// Hand out one key package per device of `username`'s account, deleting each.
/// `except` is the asking device, which is left out when people ask for their
/// own username (to add their *other* devices).
pub fn claim_key_packages(
    conn: &Connection,
    username: &str,
    except: Option<Uuid>,
) -> Result<Vec<(Uuid, Vec<u8>)>, Refusal> {
    let account = crate::db::enabled_account_id(conn, username)
        .map_err(|e| Refusal::Storage(e.to_string()))?
        .ok_or(Refusal::NoSuchPerson)?;
    let devices: Vec<String> = {
        let mut stmt = conn.prepare("SELECT device FROM e2ee_devices WHERE account = ?1")?;
        let rows = stmt.query_map(params![account], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut out = Vec::new();
    for dev in devices {
        let Ok(id) = Uuid::parse_str(&dev) else {
            continue;
        };
        if Some(id) == except {
            continue;
        }
        let row: Option<(i64, Vec<u8>)> = conn
            .query_row(
                "SELECT id, bytes FROM e2ee_key_packages WHERE device = ?1 ORDER BY id LIMIT 1",
                params![dev],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((row_id, bytes)) = row {
            conn.execute(
                "DELETE FROM e2ee_key_packages WHERE id = ?1",
                params![row_id],
            )?;
            out.push((id, bytes));
        }
    }
    Ok(out)
}

/// Register a new chat with its creator as the only member. For a direct chat,
/// answer with the existing chat's id if these two people already have one.
pub fn create_group(
    conn: &Connection,
    account: &str,
    group: &[u8],
    direct_with: Option<&str>,
) -> Result<Option<Vec<u8>>, Refusal> {
    if group.is_empty() || group.len() > MAX_GROUP_ID_BYTES {
        return Err(Refusal::TooLarge);
    }
    let pair = match direct_with {
        Some(name) => {
            let other = crate::db::enabled_account_id(conn, name)
                .map_err(|e| Refusal::Storage(e.to_string()))?
                .ok_or(Refusal::NoSuchPerson)?;
            if other == account {
                return Err(Refusal::DirectIsTwo);
            }
            let (a, b) = if account < other.as_str() {
                (account.to_string(), other)
            } else {
                (other, account.to_string())
            };
            let existing: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT id FROM e2ee_groups WHERE direct_a = ?1 AND direct_b = ?2",
                    params![a, b],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = existing {
                return Ok(Some(id));
            }
            Some((a, b))
        }
        None => None,
    };
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM e2ee_groups WHERE id = ?1",
            params![group],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        return Err(Refusal::GroupExists);
    }
    let (a, b) = pair.map_or((None, None), |(a, b)| (Some(a), Some(b)));
    conn.execute(
        "INSERT INTO e2ee_groups (id, created_by, direct_a, direct_b) VALUES (?1, ?2, ?3, ?4)",
        params![group, account, a, b],
    )?;
    conn.execute(
        "INSERT INTO e2ee_members (group_id, account) VALUES (?1, ?2)",
        params![group, account],
    )?;
    Ok(None)
}

/// Accounts in a chat, for delivery.
pub fn members(conn: &Connection, group: &[u8]) -> Result<Vec<String>, Refusal> {
    let mut stmt = conn.prepare("SELECT account FROM e2ee_members WHERE group_id = ?1")?;
    let rows = stmt.query_map(params![group], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn is_member(conn: &Connection, group: &[u8], account: &str) -> Result<bool, Refusal> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM e2ee_members WHERE group_id = ?1 AND account = ?2",
            params![group, account],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Who an invitation is for, as sent.
pub struct WelcomeFor<'a> {
    pub devices: &'a [Uuid],
    pub members: &'a [String],
    pub bytes: &'a [u8],
}

/// Append a sealed entry to a chat's log, applying rules 2 and 3.
#[allow(clippy::too_many_arguments)]
pub fn append(
    conn: &Connection,
    account: &str,
    sender_name: &str,
    device: Uuid,
    group: &[u8],
    epoch: u64,
    is_commit: bool,
    message: &[u8],
    welcome: Option<WelcomeFor<'_>>,
    now_ms: u64,
) -> Result<Appended, Refusal> {
    if message.is_empty() || message.len() > MAX_MESSAGE_BYTES {
        return Err(Refusal::TooLarge);
    }
    if welcome
        .as_ref()
        .is_some_and(|w| w.bytes.len() > MAX_MESSAGE_BYTES)
    {
        return Err(Refusal::TooLarge);
    }
    let tx = conn.unchecked_transaction()?;
    let row: Option<(i64, i64, Option<String>)> = tx
        .query_row(
            "SELECT epoch, head, direct_a FROM e2ee_groups WHERE id = ?1",
            params![group],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((current_epoch, head, direct)) = row else {
        return Err(Refusal::NoSuchGroup);
    };
    if !is_member(&tx, group, account)? {
        return Err(Refusal::NotAMember);
    }
    if is_commit && epoch != current_epoch as u64 {
        return Err(Refusal::StaleEpoch);
    }

    // Resolve who the invitation adds before writing anything, so a bad one
    // refuses the whole entry rather than half of it.
    let mut added: Vec<String> = Vec::new();
    if let Some(w) = &welcome {
        if !is_commit {
            return Err(Refusal::ForeignDevice);
        }
        for name in w.members {
            let acc = crate::db::enabled_account_id(&tx, name)
                .map_err(|e| Refusal::Storage(e.to_string()))?
                .ok_or(Refusal::NoSuchPerson)?;
            if !added.contains(&acc) {
                added.push(acc);
            }
        }
        for d in w.devices {
            let owner: Option<String> = tx
                .query_row(
                    "SELECT account FROM e2ee_devices WHERE device = ?1",
                    params![d.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            let ok = owner.is_some_and(|o| o == account || added.contains(&o));
            if !ok {
                return Err(Refusal::ForeignDevice);
            }
        }
        if direct.is_some() {
            let mut everyone = members(&tx, group)?;
            for a in &added {
                if !everyone.contains(a) {
                    everyone.push(a.clone());
                }
            }
            if everyone.len() > 2 {
                return Err(Refusal::DirectIsTwo);
            }
        }
    }

    let seq = head as u64 + 1;
    tx.execute(
        "INSERT INTO e2ee_log (group_id, seq, sender, sender_name, device, is_commit, sent_at_ms, message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            group,
            seq as i64,
            account,
            sender_name,
            device.to_string(),
            is_commit,
            now_ms as i64,
            message
        ],
    )?;
    tx.execute(
        "UPDATE e2ee_groups SET head = ?2, epoch = epoch + ?3 WHERE id = ?1",
        params![group, seq as i64, i64::from(is_commit)],
    )?;
    let mut invites = Vec::new();
    if let Some(w) = &welcome {
        for acc in &added {
            tx.execute(
                "INSERT OR IGNORE INTO e2ee_members (group_id, account) VALUES (?1, ?2)",
                params![group, acc],
            )?;
        }
        // Whoever was waiting to be let into this channel is in now.
        crate::workspaces::settle_joins(&tx, group, &added)?;
        for d in w.devices {
            tx.execute(
                "INSERT INTO e2ee_invites (device, group_id, sender_name, after, welcome)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![d.to_string(), group, sender_name, seq as i64, w.bytes],
            )?;
            invites.push(Invite {
                device: *d,
                group: group.to_vec(),
                from: sender_name.to_string(),
                after: seq,
                welcome: w.bytes.to_vec(),
            });
        }
    }
    // The writer holds the chat, and so will every device its invitation names.
    hold(&tx, group, device)?;
    if let Some(w) = &welcome {
        for d in w.devices {
            hold(&tx, group, *d)?;
        }
    }
    let everyone = members(&tx, group)?;
    tx.commit()?;
    Ok(Appended {
        entry: Entry {
            group: group.to_vec(),
            seq,
            sender: account.to_string(),
            sender_name: sender_name.to_string(),
            device,
            is_commit,
            sent_at_ms: now_ms,
            message: message.to_vec(),
        },
        members: everyone,
        invites,
    })
}

/// Everything a device has not seen: log entries after each cursor (for chats
/// the account is in), then the invitations still waiting for it.
///
/// `blocked` is the account's block list, by lowercase username; application
/// entries from those senders are left out, commits never are (rule 4).
pub fn sync(
    conn: &Connection,
    account: &str,
    device: Uuid,
    cursors: &[(Vec<u8>, u64)],
    blocked: &std::collections::HashSet<String>,
) -> Result<(Vec<Entry>, Vec<Invite>), Refusal> {
    let mut entries = Vec::new();
    for (group, after) in cursors {
        if !is_member(conn, group, account)? {
            continue;
        }
        let mut stmt = conn.prepare(
            "SELECT seq, sender, sender_name, device, is_commit, sent_at_ms, message
             FROM e2ee_log WHERE group_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![group, *after as i64, SYNC_BATCH], |r| {
            Ok(Entry {
                group: group.clone(),
                seq: r.get::<_, i64>(0)? as u64,
                sender: r.get(1)?,
                sender_name: r.get(2)?,
                device: Uuid::parse_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                is_commit: r.get(4)?,
                sent_at_ms: r.get::<_, i64>(5)? as u64,
                message: r.get(6)?,
            })
        })?;
        for e in rows {
            let e = e?;
            if !e.is_commit && blocked.contains(&e.sender_name.to_lowercase()) {
                continue;
            }
            entries.push(e);
        }
    }
    let dev = device.to_string();
    // A chat that appears in the cursors has been joined: its invitation is
    // spent. Dropped here rather than on delivery, so a device that crashed
    // between receiving an invitation and saving the result gets it again.
    for (group, _) in cursors {
        conn.execute(
            "DELETE FROM e2ee_invites WHERE device = ?1 AND group_id = ?2",
            params![dev, group],
        )?;
        if is_member(conn, group, account)? {
            hold(conn, group, device)?;
        }
    }
    let invites = {
        let mut stmt = conn.prepare(
            "SELECT group_id, sender_name, after, welcome FROM e2ee_invites WHERE device = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![dev], |r| {
            Ok(Invite {
                device,
                group: r.get(0)?,
                from: r.get(1)?,
                after: r.get::<_, i64>(2)? as u64,
                welcome: r.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    Ok((entries, invites))
}

/// Record that `device` holds `group`.
pub fn hold(conn: &Connection, group: &[u8], device: Uuid) -> Result<(), Refusal> {
    conn.execute(
        "INSERT OR IGNORE INTO e2ee_group_devices (group_id, device) VALUES (?1, ?2)",
        params![group, device.to_string()],
    )?;
    Ok(())
}

/// How long a device in a chat is given to add a missing one before another
/// is asked. Each ask spends one of the missing device's key packages.
pub const ASK_AGAIN_MS: u64 = 2 * 60 * 1000;

/// A device not seen for this long is not added to chats any more: it is most
/// likely a browser whose storage was cleared, and every add would leave one
/// more member nobody answers for.
pub const STALE_DEVICE_DAYS: u32 = 30;

/// One chat with devices of its members missing from it.
#[derive(Debug, Clone, PartialEq)]
pub struct MissingDevices {
    pub group: Vec<u8>,
    /// Devices that hold the chat, any of which can add the missing ones.
    pub holders: Vec<Uuid>,
    /// The missing devices and the usernames they belong to.
    pub missing: Vec<(Uuid, String)>,
}

/// Every chat `account` is in (or just `only`) that a member device is missing
/// from: one of the member's devices signed in after the chat started, or had
/// no key package left when it did. Devices asked about in the last
/// [`ASK_AGAIN_MS`], devices with no package to claim and stale devices are
/// left out, as is a chat no device holds.
pub fn missing_devices(
    conn: &Connection,
    account: &str,
    only: Option<&[u8]>,
    now_ms: u64,
) -> Result<Vec<MissingDevices>, Refusal> {
    let groups: Vec<Vec<u8>> = match only {
        Some(g) => vec![g.to_vec()],
        None => {
            let mut stmt = conn.prepare("SELECT group_id FROM e2ee_members WHERE account = ?1")?;
            let rows = stmt.query_map(params![account], |r| r.get::<_, Vec<u8>>(0))?;
            rows.collect::<Result<_, _>>()?
        }
    };
    let mut out = Vec::new();
    for group in groups {
        let mut stmt = conn.prepare(
            "SELECT d.device, a.username FROM e2ee_members m
               JOIN e2ee_devices d ON d.account = m.account
               JOIN accounts a ON a.id = m.account AND a.disabled = 0
             WHERE m.group_id = ?1
               AND d.last_seen >= datetime('now', ?3)
               AND NOT EXISTS (SELECT 1 FROM e2ee_group_devices h
                               WHERE h.group_id = ?1 AND h.device = d.device)
               AND NOT EXISTS (SELECT 1 FROM e2ee_device_asks k
                               WHERE k.group_id = ?1 AND k.device = d.device AND k.asked_ms > ?2)
               AND EXISTS (SELECT 1 FROM e2ee_key_packages p WHERE p.device = d.device)
             ORDER BY d.device",
        )?;
        let since = now_ms.saturating_sub(ASK_AGAIN_MS) as i64;
        let stale = format!("-{STALE_DEVICE_DAYS} days");
        let rows = stmt.query_map(params![group, since, stale], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let missing: Vec<(Uuid, String)> = rows
            .filter_map(Result::ok)
            .filter_map(|(d, name)| Uuid::parse_str(&d).ok().map(|d| (d, name)))
            .collect();
        if missing.is_empty() {
            continue;
        }
        // The missing devices' own people first: your laptop adds your phone
        // before the person you are talking to is asked to.
        let owners: Vec<String> = missing.iter().map(|(_, n)| n.to_lowercase()).collect();
        let mut stmt = conn.prepare(
            "SELECT h.device, lower(a.username) FROM e2ee_group_devices h
               JOIN e2ee_devices d ON d.device = h.device
               JOIN accounts a ON a.id = d.account
             WHERE h.group_id = ?1 ORDER BY h.device",
        )?;
        let mut rows: Vec<(String, String)> = stmt
            .query_map(params![group], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(Result::ok)
            .collect();
        rows.sort_by_key(|(_, owner)| !owners.contains(owner));
        let holders: Vec<Uuid> = rows
            .into_iter()
            .filter_map(|(d, _)| Uuid::parse_str(&d).ok())
            .collect();
        if holders.is_empty() {
            continue;
        }
        out.push(MissingDevices {
            group,
            holders,
            missing,
        });
    }
    Ok(out)
}

/// Claim one key package for each of `devices`, to be added to `group`, and
/// record the ask so it is not repeated for [`ASK_AGAIN_MS`]. A device with no
/// package left is absent from the answer.
pub fn claim_for_add(
    conn: &Connection,
    group: &[u8],
    devices: &[Uuid],
    now_ms: u64,
) -> Result<Vec<(Uuid, Vec<u8>)>, Refusal> {
    let tx = conn.unchecked_transaction()?;
    let mut out = Vec::new();
    for d in devices {
        let dev = d.to_string();
        let row: Option<(i64, Vec<u8>)> = tx
            .query_row(
                "SELECT id, bytes FROM e2ee_key_packages WHERE device = ?1 ORDER BY id LIMIT 1",
                params![dev],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((row_id, bytes)) = row else {
            continue;
        };
        tx.execute(
            "DELETE FROM e2ee_key_packages WHERE id = ?1",
            params![row_id],
        )?;
        tx.execute(
            "INSERT INTO e2ee_device_asks (group_id, device, asked_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT (group_id, device) DO UPDATE SET asked_ms = ?3",
            params![group, dev, now_ms as i64],
        )?;
        out.push((*d, bytes));
    }
    tx.commit()?;
    Ok(out)
}

/// Remove an account's devices, key packages, pending invitations and
/// memberships. The chats and their sealed logs stay: they are the other
/// members' history, and nobody could read the deleted person's part of it
/// without keys that went with their devices.
pub fn erase_account(conn: &Connection, account: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM e2ee_key_packages WHERE device IN (SELECT device FROM e2ee_devices WHERE account = ?1)",
        params![account],
    )?;
    conn.execute(
        "DELETE FROM e2ee_invites WHERE device IN (SELECT device FROM e2ee_devices WHERE account = ?1)",
        params![account],
    )?;
    for table in ["e2ee_group_devices", "e2ee_device_asks"] {
        conn.execute(
            &format!(
                "DELETE FROM {table} WHERE device IN (SELECT device FROM e2ee_devices WHERE account = ?1)"
            ),
            params![account],
        )?;
    }
    conn.execute(
        "DELETE FROM e2ee_devices WHERE account = ?1",
        params![account],
    )?;
    conn.execute(
        "DELETE FROM e2ee_members WHERE account = ?1",
        params![account],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn db() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut c).unwrap();
        c
    }

    fn account(c: &Connection, name: &str) -> String {
        crate::auth::create_account(c, name, "correct horse battery")
            .unwrap()
            .to_string()
    }

    struct Two {
        c: Connection,
        alice: String,
        bob: String,
        ad: Uuid,
        bd: Uuid,
    }

    fn two() -> Two {
        let c = db();
        let alice = account(&c, "alice");
        let bob = account(&c, "bob");
        let (ad, bd) = (Uuid::new_v4(), Uuid::new_v4());
        register_device(&c, &alice, ad, &[b"a1".to_vec()]).unwrap();
        register_device(&c, &bob, bd, &[b"b1".to_vec(), b"b2".to_vec()]).unwrap();
        Two {
            c,
            alice,
            bob,
            ad,
            bd,
        }
    }

    fn add_bob(t: &Two, g: &[u8]) -> Appended {
        append(
            &t.c,
            &t.alice,
            "alice",
            t.ad,
            g,
            0,
            true,
            b"commit",
            Some(WelcomeFor {
                devices: &[t.bd],
                members: &["bob".to_string()],
                bytes: b"welcome",
            }),
            1,
        )
        .unwrap()
    }

    #[test]
    fn a_key_package_is_handed_out_once() {
        let t = two();
        let first = claim_key_packages(&t.c, "bob", None).unwrap();
        let second = claim_key_packages(&t.c, "bob", None).unwrap();
        let third = claim_key_packages(&t.c, "bob", None).unwrap();
        assert_eq!(first, vec![(t.bd, b"b1".to_vec())]);
        assert_eq!(second, vec![(t.bd, b"b2".to_vec())]);
        assert!(
            third.is_empty(),
            "a used package must never be handed out again"
        );
    }

    #[test]
    fn a_device_cannot_be_claimed_by_a_second_account() {
        let t = two();
        assert_eq!(
            register_device(&t.c, &t.bob, t.ad, &[]),
            Err(Refusal::DeviceTaken)
        );
    }

    #[test]
    fn a_second_direct_chat_between_two_people_points_at_the_first() {
        let t = two();
        assert_eq!(
            create_group(&t.c, &t.alice, b"g1", Some("bob")).unwrap(),
            None
        );
        assert_eq!(
            create_group(&t.c, &t.bob, b"g2", Some("alice")).unwrap(),
            Some(b"g1".to_vec())
        );
    }

    #[test]
    fn of_two_commits_in_one_epoch_only_the_first_is_accepted() {
        let t = two();
        create_group(&t.c, &t.alice, b"g", None).unwrap();
        add_bob(&t, b"g");
        let stale = append(&t.c, &t.bob, "bob", t.bd, b"g", 0, true, b"late", None, 2);
        assert_eq!(stale.unwrap_err(), Refusal::StaleEpoch);
        let fresh = append(&t.c, &t.bob, "bob", t.bd, b"g", 1, true, b"ok", None, 2).unwrap();
        assert_eq!(fresh.entry.seq, 2);
    }

    #[test]
    fn only_members_may_write() {
        let t = two();
        create_group(&t.c, &t.alice, b"g", None).unwrap();
        let r = append(&t.c, &t.bob, "bob", t.bd, b"g", 0, false, b"hi", None, 1);
        assert_eq!(r.unwrap_err(), Refusal::NotAMember);
    }

    #[test]
    fn an_invitation_cannot_name_somebody_elses_device() {
        let t = two();
        let carol = account(&t.c, "carol");
        let cd = Uuid::new_v4();
        register_device(&t.c, &carol, cd, &[]).unwrap();
        create_group(&t.c, &t.alice, b"g", None).unwrap();
        // Says it adds bob, but addresses carol's device.
        let r = append(
            &t.c,
            &t.alice,
            "alice",
            t.ad,
            b"g",
            0,
            true,
            b"commit",
            Some(WelcomeFor {
                devices: &[cd],
                members: &["bob".to_string()],
                bytes: b"w",
            }),
            1,
        );
        assert_eq!(r.unwrap_err(), Refusal::ForeignDevice);
    }

    #[test]
    fn a_direct_chat_stays_between_two_people() {
        let t = two();
        let carol = account(&t.c, "carol");
        let cd = Uuid::new_v4();
        register_device(&t.c, &carol, cd, &[]).unwrap();
        create_group(&t.c, &t.alice, b"g", Some("bob")).unwrap();
        add_bob(&t, b"g");
        let r = append(
            &t.c,
            &t.alice,
            "alice",
            t.ad,
            b"g",
            1,
            true,
            b"commit",
            Some(WelcomeFor {
                devices: &[cd],
                members: &["carol".to_string()],
                bytes: b"w",
            }),
            2,
        );
        assert_eq!(r.unwrap_err(), Refusal::DirectIsTwo);
    }

    #[test]
    fn sync_replays_after_the_cursor_and_resends_an_invitation_until_joined() {
        let t = two();
        create_group(&t.c, &t.alice, b"g", None).unwrap();
        let added = add_bob(&t, b"g");
        assert_eq!(added.members.len(), 2);
        append(
            &t.c, &t.alice, "alice", t.ad, b"g", 1, false, b"m1", None, 3,
        )
        .unwrap();
        let none = HashSet::new();

        // Not joined yet: the invitation, and no entries (no cursor for the chat).
        let (entries, invites) = sync(&t.c, &t.bob, t.bd, &[], &none).unwrap();
        assert!(entries.is_empty());
        assert_eq!(invites.len(), 1);
        assert_eq!(invites[0].after, 1);
        // Joined, reading from after the commit that added him.
        let (entries, invites) = sync(&t.c, &t.bob, t.bd, &[(b"g".to_vec(), 1)], &none).unwrap();
        assert_eq!(entries.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2]);
        assert!(invites.is_empty(), "a joined chat's invitation is spent");
    }

    #[test]
    fn a_block_withholds_messages_but_never_commits() {
        let t = two();
        create_group(&t.c, &t.alice, b"g", None).unwrap();
        add_bob(&t, b"g");
        append(
            &t.c, &t.alice, "alice", t.ad, b"g", 1, false, b"msg", None, 2,
        )
        .unwrap();
        append(
            &t.c, &t.alice, "alice", t.ad, b"g", 1, true, b"commit2", None, 3,
        )
        .unwrap();
        let blocked: HashSet<String> = ["alice".to_string()].into();
        let (entries, _) = sync(&t.c, &t.bob, t.bd, &[(b"g".to_vec(), 0)], &blocked).unwrap();
        let kinds: Vec<(u64, bool)> = entries.iter().map(|e| (e.seq, e.is_commit)).collect();
        assert_eq!(kinds, vec![(1, true), (3, true)]);
    }

    #[test]
    fn a_non_member_reads_nothing() {
        let t = two();
        create_group(&t.c, &t.alice, b"g", None).unwrap();
        append(
            &t.c, &t.alice, "alice", t.ad, b"g", 0, false, b"private", None, 1,
        )
        .unwrap();
        let (entries, _) =
            sync(&t.c, &t.bob, t.bd, &[(b"g".to_vec(), 0)], &HashSet::new()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn deleting_an_account_removes_its_devices_and_memberships() {
        let t = two();
        create_group(&t.c, &t.alice, b"g", None).unwrap();
        add_bob(&t, b"g");
        erase_account(&t.c, &t.bob).unwrap();
        assert_eq!(members(&t.c, b"g").unwrap(), vec![t.alice.clone()]);
        assert_eq!(
            claim_key_packages(&t.c, "bob", None).unwrap_or_default(),
            Vec::new()
        );
    }

    #[test]
    fn a_device_missing_from_a_chat_is_found_asked_about_once_and_settled_by_its_invitation() {
        let t = two();
        let g = b"chat".to_vec();
        create_group(&t.c, &t.alice, &g, None).unwrap();
        hold(&t.c, &g, t.ad).unwrap();
        add_bob(&t, &g);
        assert_eq!(missing_devices(&t.c, &t.alice, None, 10).unwrap(), vec![]);

        // Alice signs in on a phone.
        let phone = Uuid::new_v4();
        register_device(&t.c, &t.alice, phone, &[b"p1".to_vec(), b"p2".to_vec()]).unwrap();
        let found = missing_devices(&t.c, &t.bob, None, 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].missing, vec![(phone, "alice".to_string())]);
        assert!(found[0].holders.contains(&t.ad) && found[0].holders.contains(&t.bd));

        // Asked once; not again until ASK_AGAIN_MS has passed.
        assert_eq!(claim_for_add(&t.c, &g, &[phone], 10).unwrap().len(), 1);
        assert_eq!(missing_devices(&t.c, &t.alice, None, 20).unwrap(), vec![]);
        let later = 10 + ASK_AGAIN_MS + 1;
        assert_eq!(
            missing_devices(&t.c, &t.alice, None, later).unwrap().len(),
            1
        );

        // The commit whose invitation names it settles it for good.
        append(
            &t.c,
            &t.alice,
            "alice",
            t.ad,
            &g,
            1,
            true,
            b"commit",
            Some(WelcomeFor {
                devices: &[phone],
                members: &["alice".to_string()],
                bytes: b"welcome",
            }),
            2,
        )
        .unwrap();
        assert_eq!(
            missing_devices(&t.c, &t.alice, None, later).unwrap(),
            vec![]
        );
    }
}
