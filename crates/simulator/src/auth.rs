//! Server-side authentication (ADR-016): local accounts with argon2id password
//! hashing, plus an in-memory session store.
//!
//! v0 scope: accounts persist in SQLite; sessions are in-memory (dev), so tokens
//! are valid only for the lifetime of a sim process. Persisting sessions (hashed)
//! so tokens survive a restart is a later step.
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
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use uuid::Uuid;

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
/// so a player who reconnects daily is never asked for a password again, while a
/// token copied out of a `user.json` and left in a drawer stops working. Sessions
/// are in-memory, so the outer bound is still the process lifetime.
pub const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

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
    /// When this token was issued or last used. The sweep and the TTL both read it.
    touched: Instant,
}

/// In-memory session store (ADR-016 v0): opaque token → (principal, username).
///
/// It used to be `issue` and `resolve` and nothing else — no expiry, no
/// revocation, no eviction. A token was therefore valid for the whole process
/// lifetime, which for a long-lived server is "forever", and the client persists
/// it in plaintext in `user.json`; and because nothing was ever removed, the map
/// was a slow leak keyed by connection count.
pub struct Sessions {
    by_token: Mutex<HashMap<String, Session>>,
    /// Configurable so a test can assert expiry in milliseconds rather than
    /// waiting a day or mocking the clock.
    ttl: Duration,
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
        }
    }

    /// Issue a fresh random opaque token bound to a principal + its username.
    pub fn issue(&self, principal: Uuid, username: &str) -> String {
        let token = random_token();
        let mut map = self.by_token.lock().unwrap();
        // Sweep on the way in: `resolve` only ever sees the token it was asked
        // about, so a token nobody ever presents again would otherwise sit there
        // for the life of the process.
        map.retain(|_, s| s.touched.elapsed() < self.ttl);
        if map.len() >= MAX_SESSIONS {
            evict_oldest(&mut map);
        }
        map.insert(
            token.clone(),
            Session {
                principal,
                username: username.to_string(),
                touched: Instant::now(),
            },
        );
        token
    }

    /// Resolve a token to its (principal, username), if still valid.
    ///
    /// Expired tokens are removed as they are found — the first presentation
    /// after the TTL is both the rejection and the cleanup.
    pub fn resolve(&self, token: &str) -> Option<(Uuid, String)> {
        let mut map = self.by_token.lock().unwrap();
        let session = map.get_mut(token)?;
        if session.touched.elapsed() >= self.ttl {
            map.remove(token);
            return None;
        }
        session.touched = Instant::now();
        Some((session.principal, session.username.clone()))
    }

    /// Invalidate a token immediately (sign-out, or an operator revoking access).
    ///
    /// Deliberately **not** called when a connection drops: the client's whole
    /// reason for persisting a token is the silent reconnect *after* a
    /// disconnect, so revoking there would make every restart of the client ask
    /// for a password again.
    pub fn revoke(&self, token: &str) {
        self.by_token.lock().unwrap().remove(token);
    }

    /// Live token count, for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_token.lock().unwrap().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Drop the least recently used entry. Linear, and that is fine: it runs only at
/// the ceiling, which a healthy server never reaches.
fn evict_oldest(map: &mut HashMap<String, Session>) {
    let Some(oldest) = map
        .iter()
        .min_by_key(|(_, s)| s.touched)
        .map(|(t, _)| t.clone())
    else {
        return;
    };
    map.remove(&oldest);
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
             created_at TEXT NOT NULL DEFAULT (datetime('now')));",
        )
        .unwrap();
        conn
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
