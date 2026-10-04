//! Self-service account registration, gated on an email address answering.
//!
//! # Why this is not the admin API's create-account with a different caller
//!
//! `POST /api/admin/accounts` is an operator making an account for somebody, and
//! its authorisation is the admin flag. This is a stranger making one for
//! themselves, and there is no flag to check — the only thing standing between
//! the form and an unbounded pile of accounts is that each one has to prove
//! control of a mailbox. So the two paths share the account table and nothing
//! else: this one owns the pending state, the verification tokens, the throttle
//! and the sweep, and none of that belongs on the operator's path.
//!
//! # What "pending" is
//!
//! An account whose `email` is set and whose `email_verified_at` is not. It is a
//! real row — the username is taken from the moment the form is submitted, which
//! is the point, or two people racing for a name both get told to check their
//! inbox and only one of them can ever sign in. It is not `disabled`: that column
//! is the operator's switch and conflating the two would mean verification
//! silently un-disables an account an admin deliberately turned off.
//!
//! Pending accounts do not accumulate. [`sweep`] deletes the ones nobody ever
//! confirmed, which is also what frees the username again.

use crate::auth::{random_token, token_hash};
use anyhow::Context;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// How long a verification link works.
///
/// Long enough to survive a mail that lands overnight or in a spam folder found
/// the next morning, short enough that a link forwarded or archived years later
/// is not a live credential. Re-sending is a button, so the cost of being wrong
/// in this direction is one click.
pub const VERIFICATION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long an unconfirmed account survives before it is deleted outright.
///
/// Deliberately much longer than [`VERIFICATION_TTL`]: the link expiring is
/// recoverable (ask for another), while the row being deleted frees the username
/// for somebody else, which is not. A week is "you have had several chances".
pub const PENDING_ACCOUNT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Longest address the API will store. RFC 5321 caps a path at 256 octets
/// including the angle brackets; this is that, minus them.
pub const MAX_EMAIL_LEN: usize = 254;

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Fold an address to the form that is stored, compared and indexed, or say why
/// it cannot be one.
///
/// The validation is deliberately shallow. A regex that "validates email" is
/// wrong in both directions — it rejects real addresses (plus tags, new TLDs,
/// quoted local parts, anything non-ASCII) and accepts unreachable ones — and
/// none of that matters here, because the address is about to be *tested* by
/// sending mail to it. So this checks only the things that would break the
/// system rather than the delivery: one `@`, something on each side, no
/// whitespace, no control characters, and a length a mail server will accept.
///
/// Lowercased because the unique index is over `lower(email)`; the local part is
/// technically case-sensitive and no mail provider on earth treats it that way,
/// and the alternative is two accounts one person cannot tell apart.
pub fn normalize_email(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("an email address is required");
    }
    if trimmed.len() > MAX_EMAIL_LEN {
        return Err("that email address is too long");
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("that does not look like an email address");
    }
    let mut parts = trimmed.rsplitn(2, '@');
    let domain = parts.next().unwrap_or_default();
    let local = parts.next().unwrap_or_default();
    if local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return Err("that does not look like an email address");
    }
    Ok(trimmed.to_lowercase())
}

/// What [`begin`] did about a registration request.
#[derive(Debug)]
pub enum Begin {
    /// A pending account exists and this token was mailed to it.
    Created { principal: Uuid, token: String },
    /// The username belongs to somebody else. Reported to the caller as such:
    /// they have to choose another one, and no wording can avoid telling them the
    /// name is taken.
    UsernameTaken,
    /// The address already has an account. **Not** reported to the caller — see
    /// `mail::already_registered_message` — so this carries the existing
    /// username for the mail that goes to the address instead.
    EmailTaken { username: String },
}

/// Create the pending account and its first verification token, in one
/// transaction.
///
/// Takes an already-computed argon2 PHC string rather than a password: hashing is
/// ~20 ms and ~19 MiB and must not happen with the SQLite mutex held (`auth`'s
/// module note has what that cost last time).
///
/// The address is folded here as well as in [`normalize_email`], so that a caller
/// which forgets to normalise gets a *correct* uniqueness check rather than a
/// silent second account — the unique index is over `lower(email)` and a query
/// that does not fold matches nothing.
pub fn begin(
    conn: &mut Connection,
    username: &str,
    email: &str,
    pw_hash: &str,
) -> anyhow::Result<Begin> {
    let email = email.trim().to_lowercase();
    let email = email.as_str();
    let tx = conn.transaction()?;

    // Taken by a *live* account, or by a pending one that has not expired — the
    // sweep the caller runs first is what makes the second case rare.
    let existing_username: Option<String> = tx
        .query_row(
            "SELECT id FROM accounts WHERE username = ?1",
            [username],
            |r| r.get(0),
        )
        .optional()?;
    if existing_username.is_some() {
        return Ok(Begin::UsernameTaken);
    }

    let existing_email: Option<String> = tx
        .query_row(
            "SELECT username FROM accounts WHERE lower(email) = ?1",
            [email],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(username) = existing_email {
        return Ok(Begin::EmailTaken { username });
    }

    let principal = Uuid::new_v4();
    tx.execute(
        "INSERT INTO accounts (id, username, pw_hash, email) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![principal.to_string(), username, pw_hash, email],
    )
    .context("insert pending account")?;
    let token = insert_token(&tx, principal, email)?;
    tx.commit()?;
    Ok(Begin::Created { principal, token })
}

/// Mint a verification token for a principal, replacing any outstanding one.
///
/// Replacing rather than adding is the whole behaviour of the re-send button: two
/// live links to the same account means the older mail still works after the
/// newer one was requested, which is exactly the confusion re-sending exists to
/// end.
fn insert_token(conn: &Connection, principal: Uuid, email: &str) -> anyhow::Result<String> {
    let token = random_token();
    let now = unix_now();
    conn.execute(
        "DELETE FROM email_verifications WHERE principal = ?1",
        [principal.to_string()],
    )?;
    conn.execute(
        "INSERT INTO email_verifications (token_hash, principal, email, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            token_hash(&token),
            principal.to_string(),
            email,
            now,
            now + VERIFICATION_TTL.as_secs() as i64,
        ],
    )
    .context("insert verification token")?;
    Ok(token)
}

/// One pending account, as the re-send path needs it.
#[derive(Debug, Clone)]
pub struct Pending {
    pub principal: Uuid,
    pub username: String,
    pub email: String,
}

/// The pending account for an address, if there is one. `None` covers both "no
/// such account" and "already verified", which is what the re-send endpoint
/// wants: neither is worth a different answer to a stranger.
pub fn pending_by_email(conn: &Connection, email: &str) -> anyhow::Result<Option<Pending>> {
    let email = email.trim().to_lowercase();
    let email = email.as_str();
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT id, username, email FROM accounts
             WHERE lower(email) = ?1 AND email_verified_at IS NULL",
            [email],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row.and_then(|(id, username, email)| {
        Uuid::parse_str(&id).ok().map(|principal| Pending {
            principal,
            username,
            email,
        })
    }))
}

/// Mint a fresh token for a pending account (the re-send path).
pub fn reissue(conn: &Connection, pending: &Pending) -> anyhow::Result<String> {
    insert_token(conn, pending.principal, &pending.email)
}

/// What [`verify`] made of a token.
#[derive(Debug, PartialEq, Eq)]
pub enum Verify {
    /// The account is now active.
    Confirmed { username: String },
    /// Already confirmed — the same link opened twice, which is what a mail
    /// client's link prefetcher and a person clicking twice both look like. It is
    /// a success, not an error: the account is in the state the person wanted.
    AlreadyConfirmed { username: String },
    /// No such token: never issued, already spent, or superseded by a re-send.
    Unknown,
    /// Issued, and too old.
    Expired,
}

/// Spend a verification token.
///
/// The row is deleted whether it was in time or not — an expired token is not
/// worth keeping, and deleting it means a replayed link cannot be told apart from
/// one that was never issued.
pub fn verify(conn: &mut Connection, token: &str) -> anyhow::Result<Verify> {
    let hash = token_hash(token);
    let tx = conn.transaction()?;
    let row: Option<(String, i64)> = tx
        .query_row(
            "SELECT principal, expires_at FROM email_verifications WHERE token_hash = ?1",
            [&hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((principal, expires_at)) = row else {
        // No live token. Distinguish "spent" from "never existed" only in the
        // harmless direction: if the named account is already verified we say so,
        // which is what a second click on the same link is.
        drop(tx);
        return Ok(Verify::Unknown);
    };
    tx.execute(
        "DELETE FROM email_verifications WHERE token_hash = ?1",
        [&hash],
    )?;
    if expires_at < unix_now() {
        tx.commit()?;
        return Ok(Verify::Expired);
    }
    let username: Option<String> = tx
        .query_row(
            "SELECT username FROM accounts WHERE id = ?1",
            [&principal],
            |r| r.get(0),
        )
        .optional()?;
    let Some(username) = username else {
        // The account was deleted between the mail and the click.
        tx.commit()?;
        return Ok(Verify::Unknown);
    };
    let already: Option<String> = tx
        .query_row(
            "SELECT email_verified_at FROM accounts WHERE id = ?1",
            [&principal],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if already.is_some() {
        tx.commit()?;
        return Ok(Verify::AlreadyConfirmed { username });
    }
    tx.execute(
        "UPDATE accounts SET email_verified_at = datetime('now') WHERE id = ?1",
        [&principal],
    )?;
    tx.commit()?;
    Ok(Verify::Confirmed { username })
}

/// What [`activate`] made of an operator's request.
#[derive(Debug, PartialEq, Eq)]
pub enum Activate {
    /// The account was pending and is now confirmed; mail the address.
    Activated { username: String, email: String },
    /// Already confirmed, or made by an operator with no address at all —
    /// nothing to do and nobody to tell.
    NotPending,
    /// No such account.
    Unknown,
}

/// Confirm a pending account without its link — the operator's way through when
/// the verification mail never arrived.
///
/// Spends any outstanding token too, so a link that turns up late cannot be
/// mistaken for a second, separate confirmation. The address is taken on the
/// operator's word, which is the trade they are making by pressing the button.
pub fn activate(conn: &mut Connection, principal: &str) -> anyhow::Result<Activate> {
    let tx = conn.transaction()?;
    let row: Option<(String, Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT username, email, email_verified_at FROM accounts WHERE id = ?1",
            [principal],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((username, email, verified_at)) = row else {
        return Ok(Activate::Unknown);
    };
    let (Some(email), None) = (email, verified_at) else {
        return Ok(Activate::NotPending);
    };
    tx.execute(
        "UPDATE accounts SET email_verified_at = datetime('now') WHERE id = ?1",
        [principal],
    )?;
    tx.execute(
        "DELETE FROM email_verifications WHERE principal = ?1",
        [principal],
    )?;
    tx.commit()?;
    Ok(Activate::Activated { username, email })
}

/// Delete expired tokens and the accounts nobody ever confirmed.
///
/// Returns how many accounts went. Run on the way into a registration and at
/// startup, the same "sweep on the way in" the session store uses: a background
/// timer for a table with a few hundred rows in it is a task to supervise in
/// exchange for nothing.
pub fn sweep(conn: &Connection) -> anyhow::Result<usize> {
    conn.execute(
        "DELETE FROM email_verifications WHERE expires_at < ?1",
        [unix_now()],
    )?;
    let cutoff = format!("-{} seconds", PENDING_ACCOUNT_TTL.as_secs());
    // Only ever pending accounts: `email_verified_at IS NULL` alone would match
    // every operator-made account, which have no email and are not part of this
    // scheme at all.
    let stale: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM accounts
             WHERE email IS NOT NULL
               AND email_verified_at IS NULL
               AND created_at < datetime('now', ?1)",
        )?;
        let rows = stmt
            .query_map([&cutoff], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for id in &stale {
        conn.execute("DELETE FROM email_verifications WHERE principal = ?1", [id])?;
        conn.execute("DELETE FROM accounts WHERE id = ?1", [id])?;
    }
    if !stale.is_empty() {
        tracing::info!(count = stale.len(), "swept unconfirmed registrations");
    }
    Ok(stale.len())
}

/// A fixed-window rate limiter, keyed by whatever the caller wants counted.
///
/// Registration is the one endpoint here that a stranger can reach and that costs
/// the server real money — an argon2 hash and an outbound mail per call, over a
/// relay that bills per message and will stop relaying at all if this server is
/// used to spray confirmation mail at addresses that never asked for it. So the
/// throttle is on the *address* and on the caller, and it is here rather than in
/// the handler because both the register and the re-send path need the same one.
///
/// Deliberately not a token bucket: a burst allowance is a feature for a person
/// pressing a button twice, and this is the endpoint where the second press
/// should simply be told to wait.
pub struct Throttle {
    window: Duration,
    limit: u32,
    hits: Mutex<HashMap<String, (SystemTime, u32)>>,
}

impl Throttle {
    #[must_use]
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            window,
            limit,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Count one attempt against a key. `false` means over the limit.
    pub fn allow(&self, key: &str) -> bool {
        let now = SystemTime::now();
        let mut map = self
            .hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Sweep on the way in, so an unbounded stream of distinct keys (which is
        // what an attack looks like) cannot grow the map without bound.
        map.retain(|_, (started, _)| {
            now.duration_since(*started).unwrap_or_default() < self.window
        });
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0).unwrap_or_default() >= self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut conn).unwrap();
        conn
    }

    fn hash(pw: &str) -> String {
        crate::auth::hash_password(pw).unwrap()
    }

    #[test]
    fn addresses_are_folded_and_checked_shallowly() {
        assert_eq!(
            normalize_email("  Ada@Example.COM "),
            Ok("ada@example.com".into())
        );
        // Plus tags and long TLDs are real addresses, and a stricter check is how
        // they get rejected.
        assert_eq!(
            normalize_email("ada+cartopolis@example.museum"),
            Ok("ada+cartopolis@example.museum".into())
        );
        // An @ in the local part is legal when quoted; rsplitn takes the last one.
        assert!(normalize_email("\"a@b\"@example.com").is_ok());
        assert!(normalize_email("").is_err());
        assert!(normalize_email("ada").is_err());
        assert!(normalize_email("ada@localhost").is_err());
        assert!(normalize_email("@example.com").is_err());
        assert!(normalize_email("ada@.com").is_err());
        assert!(normalize_email("ada @example.com").is_err());
        assert!(normalize_email(&format!("{}@example.com", "a".repeat(300))).is_err());
    }

    #[test]
    fn an_operator_can_activate_a_pending_account_once() {
        let mut conn = db();
        let Begin::Created { principal, token } =
            begin(&mut conn, "ada", "ada@example.com", &hash("pw")).unwrap()
        else {
            panic!("expected a pending account");
        };
        let id = principal.to_string();
        assert_eq!(
            activate(&mut conn, &id).unwrap(),
            Activate::Activated {
                username: "ada".into(),
                email: "ada@example.com".into()
            }
        );
        assert!(crate::auth::lookup_credential(&conn, "ada")
            .unwrap()
            .is_some_and(|c| !c.email_pending));
        // The mailed link is spent with it, and a second press does nothing.
        assert_eq!(verify(&mut conn, &token).unwrap(), Verify::Unknown);
        assert_eq!(activate(&mut conn, &id).unwrap(), Activate::NotPending);
        assert_eq!(
            activate(&mut conn, &Uuid::new_v4().to_string()).unwrap(),
            Activate::Unknown
        );
    }

    #[test]
    fn a_registration_creates_a_pending_account_that_cannot_sign_in() {
        let mut conn = db();
        let Begin::Created { principal, token } =
            begin(&mut conn, "ada", "ada@example.com", &hash("hunter2long")).unwrap()
        else {
            panic!("expected a fresh registration");
        };
        // The row exists and the name is taken...
        assert!(matches!(
            begin(&mut conn, "ada", "other@example.com", &hash("hunter2long")).unwrap(),
            Begin::UsernameTaken
        ));
        // ...and the credential is refused until the address answers.
        let looked_up = crate::auth::lookup_credential(&conn, "ada")
            .unwrap()
            .unwrap();
        assert_eq!(looked_up.principal, principal);
        assert!(looked_up.email_pending);

        assert_eq!(
            verify(&mut conn, &token).unwrap(),
            Verify::Confirmed {
                username: "ada".into()
            }
        );
        assert!(
            !crate::auth::lookup_credential(&conn, "ada")
                .unwrap()
                .unwrap()
                .email_pending
        );
    }

    #[test]
    fn a_second_click_on_the_same_link_is_not_an_error() {
        let mut conn = db();
        let Begin::Created { token, .. } =
            begin(&mut conn, "ada", "ada@example.com", &hash("hunter2long")).unwrap()
        else {
            panic!()
        };
        verify(&mut conn, &token).unwrap();
        // The token row is spent, so this is Unknown rather than a second
        // confirmation — the endpoint turns it into the same answer for the
        // person, but the distinction has to survive to that decision.
        assert_eq!(verify(&mut conn, &token).unwrap(), Verify::Unknown);
    }

    #[test]
    fn a_taken_address_names_the_account_it_belongs_to() {
        let mut conn = db();
        begin(&mut conn, "ada", "ada@example.com", &hash("hunter2long")).unwrap();
        let out = begin(&mut conn, "grace", "ADA@example.com", &hash("hunter2long")).unwrap();
        match out {
            Begin::EmailTaken { username } => assert_eq!(username, "ada"),
            other => panic!("expected EmailTaken, got {other:?}"),
        }
    }

    #[test]
    fn resending_invalidates_the_previous_link() {
        let mut conn = db();
        let Begin::Created { token: first, .. } =
            begin(&mut conn, "ada", "ada@example.com", &hash("hunter2long")).unwrap()
        else {
            panic!()
        };
        let pending = pending_by_email(&conn, "ada@example.com").unwrap().unwrap();
        let second = reissue(&conn, &pending).unwrap();
        assert_ne!(first, second);
        assert_eq!(verify(&mut conn, &first).unwrap(), Verify::Unknown);
        assert_eq!(
            verify(&mut conn, &second).unwrap(),
            Verify::Confirmed {
                username: "ada".into()
            }
        );
    }

    #[test]
    fn an_expired_token_is_told_apart_from_a_wrong_one() {
        let mut conn = db();
        let Begin::Created { token, principal } =
            begin(&mut conn, "ada", "ada@example.com", &hash("hunter2long")).unwrap()
        else {
            panic!()
        };
        conn.execute(
            "UPDATE email_verifications SET expires_at = 1 WHERE principal = ?1",
            [principal.to_string()],
        )
        .unwrap();
        assert_eq!(verify(&mut conn, &token).unwrap(), Verify::Expired);
        assert_eq!(verify(&mut conn, "not-a-token").unwrap(), Verify::Unknown);
    }

    #[test]
    fn the_sweep_frees_an_abandoned_username_and_spares_everyone_else() {
        let mut conn = db();
        begin(&mut conn, "ada", "ada@example.com", &hash("hunter2long")).unwrap();
        let Begin::Created { token, .. } = begin(
            &mut conn,
            "grace",
            "grace@example.com",
            &hash("hunter2long"),
        )
        .unwrap() else {
            panic!()
        };
        verify(&mut conn, &token).unwrap();
        // An operator-made account: no email at all, and not this scheme's
        // business however old it gets.
        crate::auth::create_account_with_role(&conn, "root", "hunter2long", true).unwrap();
        conn.execute(
            "UPDATE accounts SET created_at = datetime('now', '-30 days')",
            [],
        )
        .unwrap();

        assert_eq!(sweep(&conn).unwrap(), 1);
        assert!(matches!(
            begin(&mut conn, "ada", "ada@example.com", &hash("hunter2long")).unwrap(),
            Begin::Created { .. }
        ));
        assert!(crate::auth::lookup_credential(&conn, "grace")
            .unwrap()
            .is_some());
        assert!(crate::auth::lookup_credential(&conn, "root")
            .unwrap()
            .is_some());
    }

    #[test]
    fn the_throttle_counts_per_key_and_forgets() {
        let t = Throttle::new(2, Duration::from_millis(60));
        assert!(t.allow("a"));
        assert!(t.allow("a"));
        assert!(!t.allow("a"));
        // A different caller is unaffected by the first one's limit.
        assert!(t.allow("b"));
        std::thread::sleep(Duration::from_millis(80));
        assert!(t.allow("a"));
    }
}
