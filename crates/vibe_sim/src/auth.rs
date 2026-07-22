//! Server-side authentication (ADR-016): local accounts with argon2id password
//! hashing, plus an in-memory session store.
//!
//! v0 scope: accounts persist in SQLite; sessions are in-memory (dev), so tokens
//! are valid only for the lifetime of a sim process. Persisting sessions (hashed)
//! so tokens survive a restart is a later step.

use anyhow::{anyhow, Context};
use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::sync::Mutex;
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
fn verify_password(password: &str, phc: &str) -> bool {
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

/// Verify username + password. Returns the principal on success, `None` on bad
/// credentials or unknown/disabled user (indistinguishable to the caller).
pub fn verify_credentials(
    conn: &Connection,
    username: &str,
    password: &str,
) -> anyhow::Result<Option<Uuid>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT id, pw_hash FROM accounts WHERE username = ?1 AND disabled = 0",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, hash)) = row else {
        return Ok(None);
    };
    Ok(verify_password(password, &hash)
        .then(|| Uuid::parse_str(&id).ok())
        .flatten())
}

/// In-memory session store (ADR-016 v0): opaque token → (principal, username).
#[derive(Default)]
pub struct Sessions {
    by_token: Mutex<HashMap<String, (Uuid, String)>>,
}

impl Sessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a fresh random opaque token bound to a principal + its username.
    pub fn issue(&self, principal: Uuid, username: &str) -> String {
        let token = random_token();
        self.by_token
            .lock()
            .unwrap()
            .insert(token.clone(), (principal, username.to_string()));
        token
    }

    /// Resolve a token to its (principal, username), if still valid.
    pub fn resolve(&self, token: &str) -> Option<(Uuid, String)> {
        self.by_token.lock().unwrap().get(token).cloned()
    }
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
}
