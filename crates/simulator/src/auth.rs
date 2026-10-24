//! Server-side authentication (ADR-016): local accounts with argon2id password
//! hashing, plus the session store.
//!
//! Accounts persist in SQLite; sessions live in memory and are **written through
//! to SQLite** (hashed — see [`Sessions`]) so a token survives a sim restart.
//! They did not until 2026-08: every redeploy of the hosted sim silently signed
//! everybody out, which read on the client as "stay signed in" being broken.
//!
//! # Hashing is deliberately kept away from everything else
//!
//! An argon2id verify is tens of milliseconds of CPU and ~19 MiB of scratch
//! memory *by design* — that is the whole point of the algorithm, and it makes it
//! the one operation here that must never be performed while holding a lock
//! anything else wants. It was: the connection handler passed the live SQLite
//! `MutexGuard` into [`verify_credentials`], and the same mutex is taken by the
//! tick loop's script flush, which at the time ran **while the tick loop held the world
//! write lock** (it no longer does — `prims::PrimService::flush`).
//! A handful of unauthenticated sockets could therefore stop the simulation for
//! every connected client without ever logging in. The account row is read and the
//! guard dropped before any hashing now ([`lookup_account`]), and the hashing
//! itself is both moved off the async runtime and capped
//! ([`verify_password_bounded`]).

use anyhow::{anyhow, Context};
use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use blake2::{Blake2s256, Digest};
use cartopolis_core::permissions::PermissionSet;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::sync::Semaphore;
use uuid::Uuid;

/// Lock a mutex, surviving poisoning. The session map is consulted from the tick
/// loop and from every connection task, so a panic under this lock must cost the
/// panicking task alone — an `unwrap()` here turns one poisoned guard into a
/// panic in every later caller, which on the deployed sim is a process-wide
/// outage. The data is a plain map with no multi-step invariant, so the value
/// mid-panic is as good as any.
fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Hash a password as an argon2id PHC string for storage.
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow!("argon2 hash: {e}"))?
        .to_string())
}

/// Verify a password against a stored PHC string. Never panics on malformed input.
///
/// Public because the caller has to be able to do this *after* releasing the
/// database — see the module note.
pub fn verify_password(password: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Create an account, returning its principal id. Fails if the username exists.
pub fn create_account(conn: &Connection, username: &str, password: &str) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let hash = hash_password(password)?;
    conn.execute(
        "INSERT INTO accounts (id, username, pw_hash) VALUES (?1, ?2, ?3)",
        rusqlite::params![id.to_string(), username, hash],
    )
    .context("insert account")?;
    Ok(id)
}

/// Create the account only if the username is absent (idempotent). Returns the
/// principal id either way. Used by solo/dev mode to provision the dev account.
pub fn ensure_account(conn: &Connection, username: &str, password: &str) -> anyhow::Result<Uuid> {
    if let Some(id) = account_id(conn, username)? {
        return Ok(id);
    }
    create_account(conn, username, password)
}

fn account_id(conn: &Connection, username: &str) -> anyhow::Result<Option<Uuid>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT id FROM accounts WHERE username = ?1 AND disabled = 0",
            [username],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|s| Uuid::parse_str(&s).ok()))
}

/// The stored credential for a username, if the account exists and an operator
/// has not disabled it.
///
/// The half of [`verify_credentials`] that needs the database, split out so a
/// caller can drop the connection guard before spending 20 ms of argon2 on the
/// answer. Returning the PHC string to the caller is not a leak of anything the
/// caller could not already read — it holds the connection.
///
/// A registration awaiting its email is returned here, flagged, rather than
/// hidden: the caller has to check the password *before* it may say anything
/// about the account's state, or "confirm your email first" becomes a way for a
/// stranger to find out which usernames have unconfirmed registrations against
/// them. Both sign-in paths follow that order.
pub fn lookup_credential(
    conn: &Connection,
    username: &str,
) -> anyhow::Result<Option<StoredCredential>> {
    let row: Option<(String, String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT id, pw_hash, email, email_verified_at FROM accounts
             WHERE username = ?1 AND disabled = 0",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(row.and_then(|(id, phc, email, verified_at)| {
        Uuid::parse_str(&id).ok().map(|principal| StoredCredential {
            principal,
            phc,
            email_pending: email.is_some() && verified_at.is_none(),
        })
    }))
}

/// What [`lookup_credential`] found.
#[derive(Debug, Clone)]
pub struct StoredCredential {
    pub principal: Uuid,
    /// The argon2id PHC string to verify against.
    pub phc: String,
    /// A self-service registration whose address has not answered yet
    /// (`registration`'s module note defines it). An operator-made account has no
    /// email and is never pending.
    pub email_pending: bool,
}

/// The stored `(principal, password hash)` for a username, if the account is one
/// that may sign in at all — enabled, and not a registration still waiting on its
/// email.
///
/// The narrow accessor, for callers that have no way to *report* a pending
/// account and would otherwise treat one as signable-in. Sign-in paths want
/// [`lookup_credential`].
pub fn lookup_account(conn: &Connection, username: &str) -> anyhow::Result<Option<(Uuid, String)>> {
    Ok(lookup_credential(conn, username)?
        .filter(|c| !c.email_pending)
        .map(|c| (c.principal, c.phc)))
}

/// One account as the admin API reports it. Deliberately carries no `pw_hash`:
/// this type crosses into JSON, and the one field that must never do that is the
/// one a `SELECT *` would have brought along for free.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountRow {
    pub id: String,
    pub username: String,
    pub disabled: bool,
    pub admin: bool,
    pub created_at: String,
    /// Present only on self-registered accounts; an operator makes accounts
    /// without one. This *is* personal data, and it is on the admin surface
    /// because the operator is the person who has to answer a deletion request
    /// and needs to be able to find the account it names.
    pub email: Option<String>,
    /// `false` while a registration is still waiting on its address. Distinct
    /// from `disabled`: that is the operator's switch, this is the email gate,
    /// and an account can be stopped by either.
    pub email_pending: bool,
    /// An AI agent speaks for this account (`docs/mockups/voice-agent/`):
    /// everybody sees a badge beside its name.
    pub agent: bool,
}

/// Every account, oldest first. The store is a few hundred rows at most, so this
/// is deliberately unpaginated — a page cursor here would be a guess at a problem
/// nobody has.
pub fn list_accounts(conn: &Connection) -> anyhow::Result<Vec<AccountRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, username, disabled, admin, created_at, email, email_verified_at, agent
         FROM accounts ORDER BY created_at, username",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let email: Option<String> = r.get(5)?;
            let verified_at: Option<String> = r.get(6)?;
            Ok(AccountRow {
                id: r.get(0)?,
                username: r.get(1)?,
                disabled: r.get::<_, i64>(2)? != 0,
                admin: r.get::<_, i64>(3)? != 0,
                created_at: r.get(4)?,
                email_pending: email.is_some() && verified_at.is_none(),
                email,
                agent: r.get::<_, i64>(7)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One account by principal id.
pub fn get_account(conn: &Connection, id: &str) -> anyhow::Result<Option<AccountRow>> {
    let row = conn
        .query_row(
            "SELECT id, username, disabled, admin, created_at, email, email_verified_at, agent
             FROM accounts WHERE id = ?1",
            [id],
            |r| {
                let email: Option<String> = r.get(5)?;
                let verified_at: Option<String> = r.get(6)?;
                Ok(AccountRow {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    disabled: r.get::<_, i64>(2)? != 0,
                    admin: r.get::<_, i64>(3)? != 0,
                    created_at: r.get(4)?,
                    email_pending: email.is_some() && verified_at.is_none(),
                    email,
                    agent: r.get::<_, i64>(7)? != 0,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Whether this principal may administer accounts.
///
/// Read per request rather than carried in the session, so revoking the flag (or
/// disabling the account) takes effect on the admin's *next* call instead of
/// whenever their token happens to expire. `disabled = 0` is part of the test for
/// the same reason: a disabled admin is not an admin.
pub fn is_admin(conn: &Connection, principal: Uuid) -> anyhow::Result<bool> {
    let admin: Option<i64> = conn
        .query_row(
            "SELECT admin FROM accounts WHERE id = ?1 AND disabled = 0",
            [principal.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(admin.unwrap_or(0) != 0)
}

/// Create an account, optionally as an admin. Fails if the username is taken.
pub fn create_account_with_role(
    conn: &Connection,
    username: &str,
    password: &str,
    admin: bool,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let hash = hash_password(password)?;
    conn.execute(
        "INSERT INTO accounts (id, username, pw_hash, admin) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id.to_string(), username, hash, i64::from(admin)],
    )
    .context("insert account")?;
    Ok(id)
}

/// Replace an account's password.
pub fn set_password(conn: &Connection, id: &str, password: &str) -> anyhow::Result<bool> {
    let hash = hash_password(password)?;
    let n = conn.execute(
        "UPDATE accounts SET pw_hash = ?2 WHERE id = ?1",
        rusqlite::params![id, hash],
    )?;
    Ok(n > 0)
}

/// Enable or disable an account. A disabled account cannot log in and cannot
/// administer, but an already-connected session survives — sessions are held in
/// memory and are not indexed by principal, so this is a gate on the *next*
/// handshake. Kicking a live player is [`Sessions::revoke`]'s job and needs the
/// token.
pub fn set_disabled(conn: &Connection, id: &str, disabled: bool) -> anyhow::Result<bool> {
    let n = conn.execute(
        "UPDATE accounts SET disabled = ?2 WHERE id = ?1",
        rusqlite::params![id, i64::from(disabled)],
    )?;
    Ok(n > 0)
}

/// Grant or revoke the admin flag.
pub fn set_admin(conn: &Connection, id: &str, admin: bool) -> anyhow::Result<bool> {
    let n = conn.execute(
        "UPDATE accounts SET admin = ?2 WHERE id = ?1",
        rusqlite::params![id, i64::from(admin)],
    )?;
    Ok(n > 0)
}

/// Mark or unmark an account as one an AI agent speaks for.
pub fn set_agent(conn: &Connection, id: &str, agent: bool) -> anyhow::Result<bool> {
    let n = conn.execute(
        "UPDATE accounts SET agent = ?2 WHERE id = ?1",
        rusqlite::params![id, i64::from(agent)],
    )?;
    Ok(n > 0)
}

/// Whether an AI agent speaks for this account. False for an account that
/// is not there, or a database that cannot say.
#[must_use]
pub fn is_agent(conn: &Connection, id: &str) -> bool {
    conn.query_row("SELECT agent FROM accounts WHERE id = ?1", [id], |r| {
        r.get::<_, i64>(0)
    })
    .is_ok_and(|a| a != 0)
}

/// Delete an account outright.
pub fn delete_account(conn: &Connection, id: &str) -> anyhow::Result<bool> {
    let n = conn.execute("DELETE FROM accounts WHERE id = ?1", [id])?;
    Ok(n > 0)
}

// ── Guests in a call (`docs/notes/calls.md`, "Guests") ─────────────────────

/// Unix seconds now.
#[must_use]
pub fn unix_now_s() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Most guests one call link lets in while it lasts.
pub const MAX_GUESTS_PER_LINK: i64 = 50;

/// A code for a guest link: 10 characters a person can read aloud, 50 bits.
fn link_code() -> String {
    const ABC: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut bytes = [0u8; 10];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| ABC[usize::from(*b) % ABC.len()] as char)
        .collect()
}

/// Make a guest link to `group`'s call, asked for by `created_by`. Answers
/// the code and when it stops working (unix ms).
pub fn create_call_link(
    conn: &Connection,
    group: &[u8],
    created_by: &str,
    now_s: u64,
) -> anyhow::Result<(String, u64)> {
    let expires = now_s + cartopolis_core::CALL_GUEST_HOURS * 3600;
    let code = link_code();
    conn.execute(
        "INSERT INTO call_links (code, group_id, created_by, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![code, group, created_by, now_s as i64, expires as i64],
    )?;
    Ok((code, expires * 1000))
}

/// Why a guest could not come in by a link.
#[derive(Debug, PartialEq, Eq)]
pub enum GuestRefusal {
    /// No such link, or it has expired.
    NoLink,
    /// The link has let in [`MAX_GUESTS_PER_LINK`] already.
    Full,
}

/// A guest's account, made by a call link's `code` for a person who gave
/// `name`: it may only join the link's call, and it is gone after
/// [`cartopolis_core::CALL_GUEST_HOURS`]. Answers its id, its username (the
/// name, made unique) and the group whose call it may join.
pub fn create_guest(
    conn: &Connection,
    code: &str,
    name: &str,
    now_s: u64,
) -> anyhow::Result<Result<(Uuid, String, Vec<u8>), GuestRefusal>> {
    let link: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT group_id, expires_at FROM call_links WHERE code = ?1",
            [code.trim()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((group, expires)) = link.filter(|(_, e)| *e > now_s as i64) else {
        return Ok(Err(GuestRefusal::NoLink));
    };
    let guests: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts WHERE guest_of = ?1 AND guest_expires_at > ?2",
        rusqlite::params![group, now_s as i64],
        |r| r.get(0),
    )?;
    if guests >= MAX_GUESTS_PER_LINK {
        return Ok(Err(GuestRefusal::Full));
    }
    // Their name as they gave it, made into one word: a username travels in
    // addresses (`name@server`) and rosters, which have no room for spaces.
    let mut base: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .take(32)
        .collect();
    if base.is_empty() {
        base = "guest".into();
    }
    let id = Uuid::new_v4();
    for _ in 0..8 {
        let mut tag = [0u8; 2];
        OsRng.fill_bytes(&mut tag);
        let username = format!("{base}~{:03x}", u16::from_le_bytes(tag) & 0xfff);
        // No password: '!' is no PHC string, so a password sign-in never
        // matches it. The session from the guest hello is the only way in.
        let made = conn.execute(
            "INSERT INTO accounts (id, username, pw_hash, guest_of, guest_expires_at)
             VALUES (?1, ?2, '!guest', ?3, ?4)",
            rusqlite::params![id.to_string(), username, group, expires],
        );
        match made {
            Ok(_) => return Ok(Ok((id, username, group))),
            // Taken: draw another tag.
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(anyhow!("could not find a free guest name"))
}

/// What an account that just signed in is, for a call's guests.
#[derive(Debug, PartialEq, Eq)]
pub enum Standing {
    /// An ordinary account.
    Member,
    /// A guest, who may only join the call of this group.
    Guest(Vec<u8>),
    /// A guest past its time, or an account that no longer exists while a
    /// session for it does (sessions outlive a swept guest): refused.
    Gone,
}

pub fn standing(conn: &Connection, principal: Uuid, now_s: u64) -> anyhow::Result<Standing> {
    let row: Option<(Option<Vec<u8>>, Option<i64>)> = conn
        .query_row(
            "SELECT guest_of, guest_expires_at FROM accounts WHERE id = ?1",
            [principal.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        None => Standing::Gone,
        Some((None, _)) => Standing::Member,
        Some((Some(group), Some(until))) if until > now_s as i64 => Standing::Guest(group),
        Some((Some(_), _)) => Standing::Gone,
    })
}

/// Delete guests and links past their time. A session left for one is
/// refused at the next sign-in ([`standing`] says `Gone`).
pub fn sweep_guests(conn: &Connection, now_s: u64) -> anyhow::Result<usize> {
    conn.execute(
        "DELETE FROM call_links WHERE expires_at <= ?1",
        [now_s as i64],
    )?;
    let n = conn.execute(
        "DELETE FROM accounts WHERE guest_of IS NOT NULL AND guest_expires_at <= ?1",
        [now_s as i64],
    )?;
    if n > 0 {
        tracing::info!(count = n, "swept expired guests");
    }
    Ok(n)
}

/// What [`delete_own_account`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfDelete {
    Deleted,
    /// Refused: this is the last enabled admin. Deleting it would leave a running
    /// server nobody can administer — the same guard the admin API applies, and
    /// it has to be here too or an admin could walk through the wall by using the
    /// in-app button instead of the API.
    LastAdmin,
    /// No such account. Reachable if two sessions delete the same account at once.
    Unknown,
}

/// Delete the caller's *own* account, and the personal traces of it.
///
/// The password check is **not** here: it needs an argon2 verify, which must not
/// run with the SQLite mutex held (see `net::authenticate` for what that cost
/// last time). The caller verifies, then calls this.
pub fn delete_own_account(
    conn: &Connection,
    principal: Uuid,
    username: &str,
) -> anyhow::Result<SelfDelete> {
    let id = principal.to_string();
    let Some(row) = get_account(conn, &id)? else {
        return Ok(SelfDelete::Unknown);
    };
    if row.admin && !row.disabled && enabled_admin_count(conn)? <= 1 {
        return Ok(SelfDelete::LastAdmin);
    }
    crate::db::erase_account_traces(conn, &id, username)?;
    if delete_account(conn, &id)? {
        Ok(SelfDelete::Deleted)
    } else {
        Ok(SelfDelete::Unknown)
    }
}

/// How many enabled admins exist.
///
/// The guard behind every destructive admin operation: deleting, disabling or
/// demoting the last one leaves a running server nobody can administer, and the
/// only repair is shell access to the box. Refusing is recoverable; locking
/// everyone out is not.
pub fn enabled_admin_count(conn: &Connection) -> anyhow::Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM accounts WHERE admin = 1 AND disabled = 0",
        [],
        |r| r.get(0),
    )?)
}

/// Create the account if absent, then make it an admin and reset its password.
///
/// The bootstrap path (`simulator --create-admin`), and idempotent on purpose: a
/// deploy script that runs it every time should converge rather than fail the
/// second time, and an operator locked out by a forgotten password should be able
/// to run the same command to get back in.
pub fn ensure_admin_account(
    conn: &Connection,
    username: &str,
    password: &str,
) -> anyhow::Result<Uuid> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM accounts WHERE username = ?1",
            [username],
            |r| r.get(0),
        )
        .optional()?;
    let id = match existing {
        Some(raw) => {
            let hash = hash_password(password)?;
            conn.execute(
                "UPDATE accounts SET pw_hash = ?2, admin = 1, disabled = 0 WHERE id = ?1",
                rusqlite::params![raw, hash],
            )?;
            Uuid::parse_str(&raw).context("stored principal is not a uuid")?
        }
        None => create_account_with_role(conn, username, password, true)?,
    };
    Ok(id)
}

/// Concurrent argon2 verifies allowed across the process.
///
/// Each one is ~19 MiB of scratch memory and a core's worth of work for tens of
/// milliseconds; `tokio::task::spawn_blocking` will happily run 512 at once, which
/// is ~10 GiB and every core. The cap is the point of the exercise — a login flood
/// should cost the flooder a queue, not cost the server its memory. Waiting on a
/// permit costs a socket and a task, nothing else, and holds no lock.
const MAX_CONCURRENT_VERIFIES: usize = 4;
static VERIFY_SLOTS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_VERIFIES);

/// Verify a password off the async runtime, with the process-wide concurrency cap
/// applied.
///
/// Takes owned `String`s because the work outlives the caller's frame: it runs on
/// a blocking thread.
pub async fn verify_password_bounded(password: String, phc: String) -> bool {
    // Only fails if the semaphore is closed, which nothing here does.
    let Ok(_permit) = VERIFY_SLOTS.acquire().await else {
        return false;
    };
    tokio::task::spawn_blocking(move || verify_password(&password, &phc))
        .await
        .unwrap_or(false)
}

/// Verify username + password. Returns the principal on success, `None` on bad
/// credentials or unknown/disabled user (indistinguishable to the caller).
///
/// Synchronous, and therefore **not** what a connection handler should call: it
/// hashes with the caller's `Connection` borrow alive. Kept for provisioning paths
/// and tests, where there is no lock to hold and no runtime to block.
pub fn verify_credentials(
    conn: &Connection,
    username: &str,
    password: &str,
) -> anyhow::Result<Option<Uuid>> {
    let Some((id, hash)) = lookup_account(conn, username)? else {
        return Ok(None);
    };
    Ok(verify_password(password, &hash).then_some(id))
}

/// How long an unused session token stays valid.
///
/// Sliding, not absolute: [`Sessions::resolve`] renews the token it just accepted,
/// so a player who keeps coming back is never asked for a password again, while a
/// token copied out of a `user.json` and left in a drawer stops working. Thirty
/// days because this is what the sign-in card's "stay signed in" tick promises —
/// it was 24 h, which signed out anyone who skipped a day and made the checkbox
/// look broken even while the sim stayed up.
pub const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// How long a fresh sign-in through another account stands in for re-typing a
/// password ([`Sessions::reauthed_recently`]).
pub const REAUTH_WINDOW: Duration = Duration::from_secs(10 * 60);

/// What `accounts.pw_hash` holds for an account made by signing in with
/// another account. Like a guest's `'!guest'` it is no PHC string, so a
/// password sign-in can never match it.
pub const NO_PASSWORD: &str = "!signin";

/// Whether a stored `pw_hash` is a real password rather than a marker.
#[must_use]
pub fn has_password(phc: &str) -> bool {
    !phc.starts_with('!')
}

/// Hard ceiling on live tokens, whatever the TTL says.
///
/// The TTL alone bounds the map only if connections are spread out in time; a
/// reconnect loop issues a token per connect and every one of them is young. When
/// the map is over this after sweeping, the oldest go — evicting the token least
/// likely to be someone's active session.
const MAX_SESSIONS: usize = 4096;

struct Session {
    principal: Uuid,
    username: String,
    /// When this token was issued or last used. The sweep and the TTL both read
    /// it. Wall-clock rather than `Instant` because a restored session's age has
    /// to be measured across processes, and `SystemTime` is the only clock that
    /// exists on both sides of a restart.
    touched: SystemTime,
}

/// One access token, as the store holds it. The raw token is not here — only
/// its hash, as the map key — so neither the map nor the table is worth stealing.
struct AccessToken {
    id: String,
    principal: Uuid,
    username: String,
    label: String,
    elevated: bool,
    created_at: u64,
    last_used: Option<u64>,
    expires_at: Option<u64>,
}

impl AccessToken {
    fn info(&self) -> AccessTokenInfo {
        AccessTokenInfo {
            id: self.id.clone(),
            label: self.label.clone(),
            elevated: self.elevated,
            created_at: self.created_at,
            last_used: self.last_used,
            expires_at: self.expires_at,
        }
    }

    fn expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|at| now >= at)
    }
}

/// An access token as its owner sees it in a list: everything but the secret.
/// Times are unix seconds, like the table. `crates/atlas/web/src/types.ts`
/// describes the same shape as `AccessTokenRow`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccessTokenInfo {
    pub id: String,
    pub label: String,
    pub elevated: bool,
    pub created_at: u64,
    pub last_used: Option<u64>,
    pub expires_at: Option<u64>,
}

/// How a request proved who it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// A sign-in: a password was typed on some device, and this is its session.
    Session,
    /// An access token the account made (`docs/notes/access-tokens.md`).
    AccessToken { id: String, elevated: bool },
}

/// Who a presented token belongs to, and what kind of token it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    pub principal: Uuid,
    pub username: String,
    pub via: Credential,
}

impl Access {
    /// What this credential may do, given what the account may do: an access
    /// token made without `elevated` acts as a player would, whatever the
    /// account's roles say (`Permission::elevated`).
    #[must_use]
    pub fn permissions(&self, account: PermissionSet) -> PermissionSet {
        match self.via {
            Credential::AccessToken {
                elevated: false, ..
            } => account.without_elevated(),
            _ => account,
        }
    }

    /// Whether a person signed in with a password for this — what managing
    /// tokens asks for, so a token can neither mint nor revoke its siblings.
    #[must_use]
    pub fn is_session(&self) -> bool {
        self.via == Credential::Session
    }
}

/// How many access tokens one account may hold. Creating one needs the password,
/// so this is not a defence against strangers; it bounds what a careless script
/// can pile up in a map every handshake consults.
pub const MAX_ACCESS_TOKENS: usize = 50;

/// How stale an access token's `last_used` may get before a use writes it back.
/// A bot makes a request per tool call, and a database write per request to keep
/// a "last used" column accurate to the second would be most of its traffic.
const LAST_USED_RESOLUTION_S: u64 = 60;

/// The session store: hash of an opaque token → (principal, username).
///
/// It used to be `issue` and `resolve` and nothing else — no expiry, no
/// revocation, no eviction. A token was therefore valid for the whole process
/// lifetime, which for a long-lived server is "forever", and the client persists
/// it in plaintext in `user.json`; and because nothing was ever removed, the map
/// was a slow leak keyed by connection count.
///
/// The in-memory map answers every lookup; a store built by [`Self::persistent`]
/// also **writes through** to the `sessions` table and reloads it at startup, so
/// the hosted sim's redeploys stop signing everybody out. Two deliberate choices
/// there: rows hold a Blake2s **hash** of the token, never the token (the client
/// keeps the raw one, so a copy of the database must not be a bag of bearer
/// credentials — same reasoning as the password hashes two tables over), and a
/// failed write is a `warn!`, not an error, because a session that lasts only
/// until the next restart is degraded, while a sign-in refused over a disk
/// hiccup is broken.
///
/// It also holds the **access tokens** (`access_tokens` table), so the two
/// places that accept a token — the wire's handshake and HTTP's `Bearer` — take
/// either kind through one [`Self::resolve_access`] and nothing else had to learn
/// a second path. They are kept in a map of their own because they age by
/// different rules: no sliding TTL, no eviction at [`MAX_SESSIONS`].
pub struct Sessions {
    by_token: Mutex<HashMap<String, Session>>,
    tokens: Mutex<HashMap<String, AccessToken>>,
    /// Configurable so a test can assert expiry in milliseconds rather than
    /// waiting a month or mocking the clock.
    ttl: Duration,
    /// Write-through target; `None` is a memory-only store (tests, and any
    /// caller that has no database to offer).
    conn: Option<Arc<Mutex<Connection>>>,
    /// When each account last proved itself again through Discord, Google or
    /// the rest (`signin`): what stands in for re-typing the password when an
    /// account has none. In memory only; a restart asks once more.
    reauthed: Mutex<HashMap<Uuid, std::time::Instant>>,
}

impl Default for Sessions {
    fn default() -> Self {
        Self::with_ttl(SESSION_TTL)
    }
}

impl Sessions {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            by_token: Mutex::new(HashMap::new()),
            tokens: Mutex::new(HashMap::new()),
            ttl,
            conn: None,
            reauthed: Mutex::new(HashMap::new()),
        }
    }

    /// A store backed by the `sessions` table: yesterday's unexpired tokens are
    /// live again, and every change from here on is written through.
    #[must_use]
    pub fn persistent(conn: Arc<Mutex<Connection>>) -> Self {
        Self::persistent_with_ttl(conn, SESSION_TTL)
    }

    /// Split out so a test can exercise restart survival and expiry-at-load
    /// without a month on the clock.
    fn persistent_with_ttl(conn: Arc<Mutex<Connection>>, ttl: Duration) -> Self {
        let mut map = HashMap::new();
        let mut tokens = HashMap::new();
        {
            let guard = locked(&conn);
            match load_sessions(&guard, ttl, &mut map) {
                Ok(expired) => {
                    tracing::info!(live = map.len(), expired, "sessions restored");
                }
                Err(e) => tracing::warn!("could not restore sessions: {e:#}"),
            }
            match load_access_tokens(&guard, &mut tokens) {
                Ok(expired) => {
                    tracing::info!(live = tokens.len(), expired, "access tokens restored");
                }
                Err(e) => tracing::warn!("could not restore access tokens: {e:#}"),
            }
        }
        Self {
            by_token: Mutex::new(map),
            tokens: Mutex::new(tokens),
            ttl,
            conn: Some(conn),
            reauthed: Mutex::new(HashMap::new()),
        }
    }

    /// Issue a fresh random opaque token bound to a principal + its username.
    /// Record that `principal` has just signed in again with another account
    /// on purpose, to confirm it is them (`signin`, purpose `reauth`).
    pub fn mark_reauthed(&self, principal: Uuid) {
        let mut map = locked(&self.reauthed);
        map.retain(|_, at| at.elapsed() < REAUTH_WINDOW);
        map.insert(principal, std::time::Instant::now());
    }

    /// Whether `principal` confirmed itself that way within [`REAUTH_WINDOW`]:
    /// the stand-in for a password re-check on an account that has none.
    #[must_use]
    pub fn reauthed_recently(&self, principal: Uuid) -> Option<std::time::Instant> {
        locked(&self.reauthed)
            .get(&principal)
            .copied()
            .filter(|at| at.elapsed() < REAUTH_WINDOW)
    }

    pub fn issue(&self, principal: Uuid, username: &str) -> String {
        let token = random_token();
        let hash = token_hash(&token);
        let mut map = locked(&self.by_token);
        // Sweep on the way in: `resolve` only ever sees the token it was asked
        // about, so a token nobody ever presents again would otherwise sit there
        // for the life of the process.
        map.retain(|_, s| age(s.touched) < self.ttl);
        let evicted = (map.len() >= MAX_SESSIONS)
            .then(|| evict_oldest(&mut map))
            .flatten();
        let now = SystemTime::now();
        map.insert(
            hash.clone(),
            Session {
                principal,
                username: username.to_string(),
                touched: now,
            },
        );
        // The map lock is never held into a database call — the two are taken in
        // this order nowhere else, and holding both is how lock cycles start.
        drop(map);
        self.db(|c| {
            // The sweep above may have dropped any number of rows; one DELETE by
            // age brings the table along rather than tracking each of them.
            c.execute(
                "DELETE FROM sessions WHERE touched < ?1",
                [cutoff(self.ttl)],
            )?;
            if let Some(evicted) = evicted {
                c.execute("DELETE FROM sessions WHERE token_hash = ?1", [evicted])?;
            }
            c.execute(
                "INSERT OR REPLACE INTO sessions (token_hash, principal, username, touched) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![hash, principal.to_string(), username, unix(now)],
            )?;
            Ok(())
        });
        token
    }

    /// Resolve a token of either kind to its (principal, username), if still valid.
    pub fn resolve(&self, token: &str) -> Option<(Uuid, String)> {
        self.resolve_access(token)
            .map(|a| (a.principal, a.username))
    }

    /// Resolve a token of either kind, saying which kind it was.
    pub fn resolve_access(&self, token: &str) -> Option<Access> {
        let hash = token_hash(token);
        if let Some((principal, username)) = self.resolve_session(&hash) {
            return Some(Access {
                principal,
                username,
                via: Credential::Session,
            });
        }
        self.resolve_token(&hash)
    }

    /// The session half of [`Self::resolve_access`].
    ///
    /// Expired tokens are removed as they are found — the first presentation
    /// after the TTL is both the rejection and the cleanup.
    fn resolve_session(&self, hash: &str) -> Option<(Uuid, String)> {
        let hash = hash.to_string();
        let mut map = locked(&self.by_token);
        let session = map.get_mut(&hash)?;
        if age(session.touched) >= self.ttl {
            map.remove(&hash);
            drop(map);
            self.db(|c| {
                c.execute("DELETE FROM sessions WHERE token_hash = ?1", [&hash])?;
                Ok(())
            });
            return None;
        }
        let now = SystemTime::now();
        session.touched = now;
        let out = (session.principal, session.username.clone());
        drop(map);
        self.db(|c| {
            c.execute(
                "UPDATE sessions SET touched = ?1 WHERE token_hash = ?2",
                rusqlite::params![unix(now), hash],
            )?;
            Ok(())
        });
        Some(out)
    }

    /// The access-token half of [`Self::resolve_access`]. An expired token is
    /// removed as it is found, as a session is.
    fn resolve_token(&self, hash: &str) -> Option<Access> {
        let now = unix(SystemTime::now());
        let mut map = locked(&self.tokens);
        let token = map.get_mut(hash)?;
        if token.expired(now) {
            let id = token.id.clone();
            map.remove(hash);
            drop(map);
            self.db(|c| {
                c.execute("DELETE FROM access_tokens WHERE id = ?1", [&id])?;
                Ok(())
            });
            return None;
        }
        let stale = token
            .last_used
            .is_none_or(|t| now.saturating_sub(t) >= LAST_USED_RESOLUTION_S);
        if stale {
            token.last_used = Some(now);
        }
        let access = Access {
            principal: token.principal,
            username: token.username.clone(),
            via: Credential::AccessToken {
                id: token.id.clone(),
                elevated: token.elevated,
            },
        };
        let id = token.id.clone();
        drop(map);
        if stale {
            self.db(|c| {
                c.execute(
                    "UPDATE access_tokens SET last_used = ?1 WHERE id = ?2",
                    rusqlite::params![now, id],
                )?;
                Ok(())
            });
        }
        Some(access)
    }

    /// Whether an access token still exists and has not expired — what an open
    /// connection made with one asks every few seconds, so revoking stops a bot
    /// that is already in the world. Touches nothing, unlike a resolve.
    #[must_use]
    pub fn access_token_live(&self, token: &str) -> bool {
        let now = unix(SystemTime::now());
        locked(&self.tokens)
            .get(&token_hash(token))
            .is_some_and(|t| !t.expired(now))
    }

    /// Make an access token for `principal`, and return the raw token — the
    /// only time it exists outside the caller's hands — with what the list will
    /// show for it. `expires_at` is unix seconds, `None` for never.
    pub fn create_access_token(
        &self,
        principal: Uuid,
        username: &str,
        label: &str,
        elevated: bool,
        expires_at: Option<u64>,
    ) -> anyhow::Result<(String, AccessTokenInfo)> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 60 {
            return Err(anyhow!("give the token a name of 1 to 60 characters"));
        }
        let now = unix(SystemTime::now());
        if expires_at.is_some_and(|at| at <= now) {
            return Err(anyhow!("that expiry date has already passed"));
        }
        let raw = random_token();
        let token = AccessToken {
            id: random_token()[..16].to_string(),
            principal,
            username: username.to_string(),
            label: label.to_string(),
            elevated,
            created_at: now,
            last_used: None,
            expires_at,
        };
        let info = token.info();
        {
            let mut map = locked(&self.tokens);
            let held = map
                .values()
                .filter(|t| t.principal == principal && !t.expired(now))
                .count();
            if held >= MAX_ACCESS_TOKENS {
                return Err(anyhow!(
                    "this account already has {MAX_ACCESS_TOKENS} access tokens; revoke one first"
                ));
            }
            map.insert(token_hash(&raw), token);
        }
        self.db(|c| {
            c.execute(
                "INSERT INTO access_tokens \
                 (id, token_hash, principal, username, label, elevated, created_at, expires_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    info.id,
                    token_hash(&raw),
                    principal.to_string(),
                    username,
                    info.label,
                    elevated,
                    now,
                    expires_at
                ],
            )?;
            Ok(())
        });
        Ok((raw, info))
    }

    /// One account's live access tokens, newest first.
    #[must_use]
    pub fn access_tokens(&self, principal: Uuid) -> Vec<AccessTokenInfo> {
        let now = unix(SystemTime::now());
        let mut out: Vec<AccessTokenInfo> = locked(&self.tokens)
            .values()
            .filter(|t| t.principal == principal && !t.expired(now))
            .map(AccessToken::info)
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        out
    }

    /// Revoke one of `principal`'s access tokens by its id. `false` when there
    /// is no such token *of theirs* — another account's id is "not found", not a
    /// different answer, so the route cannot be used to probe for ids.
    pub fn revoke_access_token(&self, principal: Uuid, id: &str) -> bool {
        let mut map = locked(&self.tokens);
        let Some(hash) = map
            .iter()
            .find(|(_, t)| t.id == id && t.principal == principal)
            .map(|(h, _)| h.clone())
        else {
            return false;
        };
        map.remove(&hash);
        drop(map);
        self.db(|c| {
            c.execute("DELETE FROM access_tokens WHERE id = ?1", [id])?;
            Ok(())
        });
        true
    }

    /// Invalidate a token immediately (sign-out, or an operator revoking access).
    ///
    /// Deliberately **not** called when a connection drops: the client's whole
    /// reason for persisting a token is the silent reconnect *after* a
    /// disconnect, so revoking there would make every restart of the client ask
    /// for a password again.
    pub fn revoke(&self, token: &str) {
        let hash = token_hash(token);
        locked(&self.by_token).remove(&hash);
        self.db(|c| {
            c.execute("DELETE FROM sessions WHERE token_hash = ?1", [&hash])?;
            Ok(())
        });
    }

    /// Invalidate every token belonging to one principal, and say how many went.
    ///
    /// A linear scan, because the map is deliberately keyed by token and by
    /// nothing else — the note on [`crate::auth::set_disabled`] calls that out as
    /// the reason disabling an account gates only the *next* handshake. Account
    /// **deletion** cannot be left that way: the account row is gone, so a
    /// surviving token would resolve to a principal with no account behind it and
    /// keep playing. It is a handful of thousand entries at most and it runs once
    /// per deletion.
    ///
    /// Access tokens go with them: deletion and a moderator's kick both mean
    /// "this account is not in the world any more", and a bot's token that
    /// survived either would put it straight back.
    pub fn revoke_principal(&self, principal: Uuid) -> usize {
        let mut map = locked(&self.by_token);
        let before = map.len();
        map.retain(|_, s| s.principal != principal);
        let mut gone = before - map.len();
        drop(map);
        let mut tokens = locked(&self.tokens);
        let before = tokens.len();
        tokens.retain(|_, t| t.principal != principal);
        gone += before - tokens.len();
        drop(tokens);
        self.db(|c| {
            c.execute(
                "DELETE FROM sessions WHERE principal = ?1",
                [principal.to_string()],
            )?;
            c.execute(
                "DELETE FROM access_tokens WHERE principal = ?1",
                [principal.to_string()],
            )?;
            Ok(())
        });
        gone
    }

    /// Live token count, for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        locked(&self.by_token).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Run a write against the backing table, if there is one. Failures are
    /// logged and swallowed on purpose — see the type-level note.
    fn db(&self, op: impl FnOnce(&Connection) -> rusqlite::Result<()>) {
        let Some(conn) = &self.conn else { return };
        if let Err(e) = op(&locked(conn)) {
            tracing::warn!("session table write failed: {e:#}");
        }
    }
}

/// Fill `map` from the `sessions` table, deleting what has already expired.
/// Returns how many expired rows were dropped.
fn load_sessions(
    conn: &Connection,
    ttl: Duration,
    map: &mut HashMap<String, Session>,
) -> anyhow::Result<usize> {
    let expired = conn.execute("DELETE FROM sessions WHERE touched < ?1", [cutoff(ttl)])?;
    let mut stmt = conn.prepare("SELECT token_hash, principal, username, touched FROM sessions")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, u64>(3)?,
        ))
    })?;
    for row in rows {
        let (hash, principal, username, touched) = row?;
        let principal = Uuid::parse_str(&principal).context("bad principal in sessions table")?;
        map.insert(
            hash,
            Session {
                principal,
                username,
                touched: std::time::UNIX_EPOCH + Duration::from_secs(touched),
            },
        );
    }
    Ok(expired)
}

/// Fill `map` from the `access_tokens` table, deleting what has expired.
/// Returns how many expired rows were dropped.
fn load_access_tokens(
    conn: &Connection,
    map: &mut HashMap<String, AccessToken>,
) -> anyhow::Result<usize> {
    let now = unix(SystemTime::now());
    let expired = conn.execute(
        "DELETE FROM access_tokens WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        [now],
    )?;
    let mut stmt = conn.prepare(
        "SELECT id, token_hash, principal, username, label, elevated, created_at, last_used, expires_at \
         FROM access_tokens",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            AccessToken {
                id: r.get(0)?,
                principal: Uuid::nil(),
                username: r.get(3)?,
                label: r.get(4)?,
                elevated: r.get::<_, i64>(5)? != 0,
                created_at: r.get(6)?,
                last_used: r.get(7)?,
                expires_at: r.get(8)?,
            },
        ))
    })?;
    for row in rows {
        let (hash, principal, mut token) = row?;
        token.principal =
            Uuid::parse_str(&principal).context("bad principal in access_tokens table")?;
        map.insert(hash, token);
    }
    Ok(expired)
}

/// How long ago, on the wall clock. A clock that moved backwards (NTP step,
/// suspend) answers zero rather than panicking — a session aged "not at all" is
/// the harmless reading of an unreadable interval.
fn age(touched: SystemTime) -> Duration {
    touched.elapsed().unwrap_or_default()
}

/// Wall-clock seconds since the epoch, the form the `touched` column stores.
fn unix(t: SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The `touched` value below which a row is expired right now.
fn cutoff(ttl: Duration) -> u64 {
    unix(SystemTime::now()).saturating_sub(ttl.as_secs())
}

/// What the map and the table are keyed by: a hash, so neither is worth stealing.
/// Blake2s over sha2 only because argon2 already brings it into the build.
pub(crate) fn token_hash(token: &str) -> String {
    let digest = Blake2s256::digest(token.as_bytes());
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Drop the least recently used entry and say which, so the caller can take the
/// table's row with it. Linear, and that is fine: it runs only at the ceiling,
/// which a healthy server never reaches.
fn evict_oldest(map: &mut HashMap<String, Session>) -> Option<String> {
    let oldest = map
        .iter()
        .min_by_key(|(_, s)| s.touched)
        .map(|(t, _)| t.clone())?;
    map.remove(&oldest);
    Some(oldest)
}

/// 256-bit random token, hex-encoded.
///
/// Shared with `registration`: a verification link is a bearer credential that
/// activates an account, so it is generated and stored exactly the way a session
/// token is rather than by a second, weaker recipe next to this one.
pub(crate) fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Connection {
        // The real migrations, not a hand-written approximation of them: this
        // block drifted behind production the moment `accounts` grew a column,
        // and a suite passing against a schema that no longer exists is worse
        // than one that fails.
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut conn).unwrap();
        conn
    }

    /// The flag is off unless someone grants it, and reading it goes through the
    /// database every time — which is what makes revoking take effect at once.
    #[test]
    fn admin_is_off_by_default_and_revocable() {
        let conn = mem_db();
        let id = ensure_account(&conn, "player", "secret").unwrap();
        assert!(
            !is_admin(&conn, id).unwrap(),
            "a new account is not an admin"
        );

        set_admin(&conn, &id.to_string(), true).unwrap();
        assert!(is_admin(&conn, id).unwrap());

        set_admin(&conn, &id.to_string(), false).unwrap();
        assert!(!is_admin(&conn, id).unwrap());
    }

    /// A disabled admin is not an admin. The API reads this on every request, so
    /// disabling an account is also how you take its admin rights away *now*
    /// rather than when its token expires.
    #[test]
    fn a_disabled_admin_cannot_administer() {
        let conn = mem_db();
        let id = ensure_admin_account(&conn, "root", "hunter22").unwrap();
        assert!(is_admin(&conn, id).unwrap());
        assert_eq!(enabled_admin_count(&conn).unwrap(), 1);

        set_disabled(&conn, &id.to_string(), true).unwrap();
        assert!(!is_admin(&conn, id).unwrap());
        assert_eq!(
            enabled_admin_count(&conn).unwrap(),
            0,
            "a disabled admin must not count towards the last-admin guard"
        );
    }

    /// `--create-admin` is run by a deploy script, so running it twice must
    /// converge rather than fail — and it doubles as the way back in after a
    /// forgotten password.
    #[test]
    fn ensure_admin_is_idempotent_and_resets_the_password() {
        let conn = mem_db();
        let first = ensure_admin_account(&conn, "root", "firstpass").unwrap();
        let again = ensure_admin_account(&conn, "root", "secondpass").unwrap();
        assert_eq!(first, again, "the principal must be stable across repairs");

        assert_eq!(
            verify_credentials(&conn, "root", "secondpass").unwrap(),
            Some(first)
        );
        assert_eq!(
            verify_credentials(&conn, "root", "firstpass").unwrap(),
            None
        );
    }

    /// Promoting an existing, disabled player re-enables them — otherwise the
    /// repair path hands back an admin who still cannot log in.
    #[test]
    fn ensure_admin_reenables_a_disabled_account() {
        let conn = mem_db();
        let id = ensure_account(&conn, "root", "somepass").unwrap();
        set_disabled(&conn, &id.to_string(), true).unwrap();

        let repaired = ensure_admin_account(&conn, "root", "newpass12").unwrap();
        assert_eq!(repaired, id);
        let row = get_account(&conn, &id.to_string()).unwrap().unwrap();
        assert!(row.admin && !row.disabled);
    }

    /// The listing is what the GUI renders, and the one column it must never
    /// carry is the hash.
    #[test]
    fn listing_accounts_reports_roles_and_no_secrets() {
        let conn = mem_db();
        ensure_admin_account(&conn, "root", "adminpass").unwrap();
        let player = ensure_account(&conn, "player", "playerpw").unwrap();
        set_disabled(&conn, &player.to_string(), true).unwrap();

        let rows = list_accounts(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        let json = serde_json::to_string(&rows).unwrap();
        assert!(
            !json.contains("$argon2"),
            "a password hash reached the API surface: {json}"
        );

        let root = rows.iter().find(|r| r.username == "root").unwrap();
        assert!(root.admin && !root.disabled);
        let player = rows.iter().find(|r| r.username == "player").unwrap();
        assert!(!player.admin && player.disabled);
    }

    #[test]
    fn deleting_an_account_removes_it() {
        let conn = mem_db();
        let id = ensure_account(&conn, "gone", "somepass").unwrap();
        assert!(delete_account(&conn, &id.to_string()).unwrap());
        assert!(get_account(&conn, &id.to_string()).unwrap().is_none());
        assert!(
            !delete_account(&conn, &id.to_string()).unwrap(),
            "deleting a missing account should report that it did nothing"
        );
    }

    #[test]
    fn ensure_is_idempotent_and_verifies() {
        let conn = mem_db();
        let a = ensure_account(&conn, "dev", "secret").unwrap();
        let b = ensure_account(&conn, "dev", "different").unwrap();
        assert_eq!(a, b, "same username returns same principal");
        assert_eq!(verify_credentials(&conn, "dev", "secret").unwrap(), Some(a));
        assert_eq!(verify_credentials(&conn, "dev", "wrong").unwrap(), None);
        assert_eq!(verify_credentials(&conn, "ghost", "secret").unwrap(), None);
    }

    #[test]
    fn sessions_roundtrip() {
        let s = Sessions::new();
        let p = Uuid::new_v4();
        let tok = s.issue(p, "dev");
        assert_eq!(s.resolve(&tok), Some((p, "dev".to_string())));
        assert_eq!(s.resolve("nope"), None);
    }

    /// A token stops working, and stops occupying memory, once it goes stale.
    ///
    /// Nothing removed a session before: the map only grew, and a token was valid
    /// for as long as the process ran — which for a server is indefinitely, for a
    /// secret the client keeps in plaintext on disk.
    #[test]
    fn a_stale_token_expires_and_is_forgotten() {
        let s = Sessions::with_ttl(Duration::from_millis(30));
        let p = Uuid::new_v4();
        let tok = s.issue(p, "dev");

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(s.resolve(&tok), None, "an expired token still resolved");
        assert!(
            s.is_empty(),
            "the expired entry should be dropped as it is rejected"
        );

        // And issuing sweeps whatever nobody ever presents again.
        let stale = s.issue(p, "dev");
        std::thread::sleep(Duration::from_millis(50));
        let fresh = s.issue(p, "dev");
        assert_eq!(s.len(), 1, "the stale token should have been swept");
        assert_eq!(s.resolve(&stale), None);
        assert!(s.resolve(&fresh).is_some());
    }

    /// Using a token renews it, so an active player is never signed out mid-play
    /// and a reconnect after a crash still works.
    #[test]
    fn using_a_token_renews_it() {
        let s = Sessions::with_ttl(Duration::from_millis(60));
        let p = Uuid::new_v4();
        let tok = s.issue(p, "dev");
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(25));
            assert!(
                s.resolve(&tok).is_some(),
                "a token in continuous use expired anyway"
            );
        }
    }

    #[test]
    fn revoking_a_token_takes_effect_immediately() {
        let s = Sessions::new();
        let p = Uuid::new_v4();
        let tok = s.issue(p, "dev");
        s.revoke(&tok);
        assert_eq!(s.resolve(&tok), None);
        assert!(s.is_empty());
    }

    /// A database with the `sessions` table, shared the way `serve` shares it.
    fn sessions_db() -> Arc<Mutex<Connection>> {
        // The real migrations, for the reason `mem_db` gives: this was a
        // hand-written `sessions` table, and it would have hidden the
        // `access_tokens` one from every test that restarts the store.
        Arc::new(Mutex::new(mem_db()))
    }

    fn table_count(conn: &Arc<Mutex<Connection>>) -> i64 {
        locked(conn)
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap()
    }

    /// **The whole reason the table exists**: the deployed sim restarts on every
    /// publish, and each restart used to sign everybody out — "stay signed in"
    /// read as broken on the client. A token issued by one store must resolve in
    /// a store built later over the same database.
    #[test]
    fn a_token_survives_a_simulator_restart() {
        let db = sessions_db();
        let p = Uuid::new_v4();
        let first = Sessions::persistent(db.clone());
        let tok = first.issue(p, "dev");
        drop(first);

        let rebooted = Sessions::persistent(db.clone());
        assert_eq!(
            rebooted.resolve(&tok),
            Some((p, "dev".to_string())),
            "a restart signed the player out"
        );

        // And what the table holds must be the hash, never the credential.
        let raw: i64 = locked(&db)
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE token_hash = ?1",
                [&tok],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw, 0, "the raw token was written to disk");
    }

    /// Expiry still holds across the reload — a stale row is dropped at boot,
    /// from the map and from the table.
    #[test]
    fn a_restart_does_not_resurrect_an_expired_token() {
        let db = sessions_db();
        let first = Sessions::persistent(db.clone());
        let tok = first.issue(Uuid::new_v4(), "dev");
        drop(first);
        locked(&db)
            .execute("UPDATE sessions SET touched = 0", [])
            .unwrap();

        let rebooted = Sessions::persistent(db.clone());
        assert_eq!(rebooted.resolve(&tok), None, "an expired token came back");
        assert!(rebooted.is_empty());
        assert_eq!(table_count(&db), 0, "the expired row was left in the table");
    }

    /// Revocation must reach the table, or a sign-out (or an operator kicking an
    /// account) is undone by the next restart.
    #[test]
    fn revocation_survives_a_restart_too() {
        let db = sessions_db();
        let p = Uuid::new_v4();
        let s = Sessions::persistent(db.clone());
        let signed_out = s.issue(p, "dev");
        let kicked = s.issue(p, "dev");
        s.revoke(&signed_out);
        assert_eq!(s.revoke_principal(p), 1);
        assert_eq!(table_count(&db), 0);
        drop(s);

        let rebooted = Sessions::persistent(db);
        assert_eq!(rebooted.resolve(&signed_out), None);
        assert_eq!(rebooted.resolve(&kicked), None);
    }

    /// The lookup/verify split has to agree with the one-shot it replaced, or the
    /// connection path and the provisioning path would disagree about who may log
    /// in.
    #[test]
    fn lookup_then_verify_matches_verify_credentials() {
        let conn = mem_db();
        let id = ensure_account(&conn, "dev", "secret").unwrap();

        let (found, phc) = lookup_account(&conn, "dev").unwrap().expect("the account");
        assert_eq!(found, id);
        assert!(verify_password("secret", &phc));
        assert!(!verify_password("wrong", &phc));
        assert!(lookup_account(&conn, "ghost").unwrap().is_none());
    }

    fn token_count(db: &Arc<Mutex<Connection>>) -> i64 {
        locked(db)
            .query_row("SELECT COUNT(*) FROM access_tokens", [], |r| r.get(0))
            .unwrap()
    }

    /// An access token resolves through the same call a session does, says
    /// which kind it is, and ignores the session TTL entirely — a bot left
    /// running for a quiet month must not find itself signed out.
    #[test]
    fn an_access_token_resolves_and_outlives_the_session_ttl() {
        let s = Sessions::with_ttl(Duration::from_millis(20));
        let p = Uuid::new_v4();
        let (raw, info) = s
            .create_access_token(p, "viberfox", "deck-2 bot", false, None)
            .unwrap();
        let session = s.issue(p, "viberfox");
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(s.resolve(&session), None, "the session has slid out");
        let access = s.resolve_access(&raw).expect("the token");
        assert_eq!(access.principal, p);
        assert_eq!(access.username, "viberfox");
        assert_eq!(
            access.via,
            Credential::AccessToken {
                id: info.id.clone(),
                elevated: false
            }
        );
        assert!(!access.is_session());
        assert!(
            s.access_tokens(p)[0].last_used.is_some(),
            "a use is recorded"
        );
    }

    /// A token on an admin account acts as a player unless it was made to carry
    /// the elevated permissions; a session is never masked.
    #[test]
    fn an_access_token_is_a_player_unless_made_elevated() {
        let s = Sessions::new();
        let p = Uuid::new_v4();
        let (plain, _) = s
            .create_access_token(p, "admin", "bot", false, None)
            .unwrap();
        let (full, _) = s
            .create_access_token(p, "admin", "ops", true, None)
            .unwrap();
        let all = PermissionSet::all();
        assert_eq!(
            s.resolve_access(&plain).unwrap().permissions(all),
            all.without_elevated()
        );
        assert_eq!(s.resolve_access(&full).unwrap().permissions(all), all);
        let session = s.issue(p, "admin");
        assert_eq!(s.resolve_access(&session).unwrap().permissions(all), all);
    }

    /// An expiry is honoured, a past one is refused at creation, and a bad label
    /// is refused rather than stored.
    #[test]
    fn an_access_token_expires_when_it_says() {
        let s = Sessions::new();
        let p = Uuid::new_v4();
        let now = unix(SystemTime::now());
        assert!(s
            .create_access_token(p, "a", "late", false, Some(now))
            .is_err());
        assert!(s.create_access_token(p, "a", "  ", false, None).is_err());
        assert!(s
            .create_access_token(p, "a", &"x".repeat(61), false, None)
            .is_err());
        let (raw, _) = s
            .create_access_token(p, "a", "brief", false, Some(now + 1))
            .unwrap();
        assert!(s.resolve(&raw).is_some());
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(s.resolve(&raw), None);
        assert!(
            s.access_tokens(p).is_empty(),
            "an expired token is not listed"
        );
    }

    /// Only the owner revokes, and somebody else's id reads as "not found".
    #[test]
    fn only_the_owner_revokes_an_access_token() {
        let s = Sessions::new();
        let (me, them) = (Uuid::new_v4(), Uuid::new_v4());
        let (raw, info) = s.create_access_token(me, "me", "bot", false, None).unwrap();
        assert!(!s.revoke_access_token(them, &info.id));
        assert!(s.resolve(&raw).is_some());
        assert!(s.revoke_access_token(me, &info.id));
        assert_eq!(s.resolve(&raw), None);
        assert!(
            !s.revoke_access_token(me, &info.id),
            "revoking twice finds nothing"
        );
    }

    /// The cap is per account.
    #[test]
    fn access_tokens_are_capped_per_account() {
        let s = Sessions::new();
        let (me, them) = (Uuid::new_v4(), Uuid::new_v4());
        for i in 0..MAX_ACCESS_TOKENS {
            s.create_access_token(me, "me", &format!("t{i}"), false, None)
                .unwrap();
        }
        assert!(s
            .create_access_token(me, "me", "one more", false, None)
            .is_err());
        assert!(s
            .create_access_token(them, "them", "theirs", false, None)
            .is_ok());
    }

    /// Tokens survive a restart as hashes, and deleting or kicking the account
    /// takes them along — on disk too.
    #[test]
    fn access_tokens_persist_and_go_with_the_account() {
        let db = sessions_db();
        let p = Uuid::new_v4();
        let first = Sessions::persistent(db.clone());
        let (raw, info) = first
            .create_access_token(p, "dev", "bot", true, None)
            .unwrap();
        drop(first);
        let stored: String = locked(&db)
            .query_row("SELECT token_hash FROM access_tokens", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, raw, "the raw token is never written down");

        let rebooted = Sessions::persistent(db.clone());
        let access = rebooted.resolve_access(&raw).expect("restored");
        assert_eq!(
            access.via,
            Credential::AccessToken {
                id: info.id,
                elevated: true
            }
        );
        assert_eq!(rebooted.revoke_principal(p), 1);
        assert_eq!(token_count(&db), 0);
        assert_eq!(rebooted.resolve(&raw), None);
    }
}
