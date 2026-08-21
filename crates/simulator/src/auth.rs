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
//! tick loop's `flush_dirty` **while the tick loop holds the world write lock**.
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

/// The stored `(principal, password hash)` for a username, if the account exists
/// and is enabled.
///
/// The half of [`verify_credentials`] that needs the database, split out so a
/// caller can drop the connection guard before spending 20 ms of argon2 on the
/// answer. Returning the PHC string to the caller is not a leak of anything the
/// caller could not already read — it holds the connection.
pub fn lookup_account(conn: &Connection, username: &str) -> anyhow::Result<Option<(Uuid, String)>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT id, pw_hash FROM accounts WHERE username = ?1 AND disabled = 0",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(id, hash)| Uuid::parse_str(&id).ok().map(|id| (id, hash))))
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
}

/// Every account, oldest first. The store is a few hundred rows at most, so this
/// is deliberately unpaginated — a page cursor here would be a guess at a problem
/// nobody has.
pub fn list_accounts(conn: &Connection) -> anyhow::Result<Vec<AccountRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, username, disabled, admin, created_at FROM accounts ORDER BY created_at, username",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(AccountRow {
                id: r.get(0)?,
                username: r.get(1)?,
                disabled: r.get::<_, i64>(2)? != 0,
                admin: r.get::<_, i64>(3)? != 0,
                created_at: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One account by principal id.
pub fn get_account(conn: &Connection, id: &str) -> anyhow::Result<Option<AccountRow>> {
    let row = conn
        .query_row(
            "SELECT id, username, disabled, admin, created_at FROM accounts WHERE id = ?1",
            [id],
            |r| {
                Ok(AccountRow {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    disabled: r.get::<_, i64>(2)? != 0,
                    admin: r.get::<_, i64>(3)? != 0,
                    created_at: r.get(4)?,
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

/// Delete an account outright.
pub fn delete_account(conn: &Connection, id: &str) -> anyhow::Result<bool> {
    let n = conn.execute("DELETE FROM accounts WHERE id = ?1", [id])?;
    Ok(n > 0)
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
pub struct Sessions {
    by_token: Mutex<HashMap<String, Session>>,
    /// Configurable so a test can assert expiry in milliseconds rather than
    /// waiting a month or mocking the clock.
    ttl: Duration,
    /// Write-through target; `None` is a memory-only store (tests, and any
    /// caller that has no database to offer).
    conn: Option<Arc<Mutex<Connection>>>,
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
            ttl,
            conn: None,
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
        {
            let guard = locked(&conn);
            match load_sessions(&guard, ttl, &mut map) {
                Ok(expired) => {
                    tracing::info!(live = map.len(), expired, "sessions restored");
                }
                Err(e) => tracing::warn!("could not restore sessions: {e:#}"),
            }
        }
        Self {
            by_token: Mutex::new(map),
            ttl,
            conn: Some(conn),
        }
    }

    /// Issue a fresh random opaque token bound to a principal + its username.
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

    /// Resolve a token to its (principal, username), if still valid.
    ///
    /// Expired tokens are removed as they are found — the first presentation
    /// after the TTL is both the rejection and the cleanup.
    pub fn resolve(&self, token: &str) -> Option<(Uuid, String)> {
        let hash = token_hash(token);
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
    pub fn revoke_principal(&self, principal: Uuid) -> usize {
        let mut map = locked(&self.by_token);
        let before = map.len();
        map.retain(|_, s| s.principal != principal);
        let gone = before - map.len();
        drop(map);
        self.db(|c| {
            c.execute(
                "DELETE FROM sessions WHERE principal = ?1",
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
fn token_hash(token: &str) -> String {
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
fn random_token() -> String {
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
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, username TEXT NOT NULL UNIQUE, \
             pw_hash TEXT NOT NULL, disabled INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT (datetime('now')), \
             admin INTEGER NOT NULL DEFAULT 0);",
        )
        .unwrap();
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
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (token_hash TEXT PRIMARY KEY, principal TEXT NOT NULL, \
             username TEXT NOT NULL, touched INTEGER NOT NULL);",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
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
}
