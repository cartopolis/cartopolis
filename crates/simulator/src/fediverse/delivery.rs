//! Activities waiting to go out, and the worker that sends them.
//!
//! Everything this server sends another server goes through a table
//! (`fedi_deliveries`) rather than straight out of the request that caused
//! it: a post must not wait on a hundred followers' servers before its author
//! sees it, a server that is down for an hour must still get it afterwards,
//! and a restart must not lose what was queued. The worker sends what is due,
//! a few at a time, and retries a failure with a growing delay — a minute, then
//! doubling, up to six hours, for about three days — before giving up.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{params, Connection};
use serde_json::Value;

use super::fetch::{host_of, Client};
use super::{blocklist, signer_url, signing, Federation};

/// Deliveries tried at once.
const BATCH: usize = 16;
/// Attempts before an activity is given up on.
const MAX_ATTEMPTS: i64 = 18;
/// How long the worker sleeps when nothing is due and nothing wakes it.
const IDLE: Duration = Duration::from_secs(30);

/// Queue `body` for `inbox`, signed as `signer` (a `fedi_keys` actor). Nothing
/// is queued for a blocked server.
pub fn enqueue(
    conn: &Connection,
    inbox: &str,
    signer: &str,
    body: &Value,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    if host_of(inbox).is_none_or(|h| blocklist::is_blocked(conn, &h).unwrap_or(true)) {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO fedi_deliveries (inbox, signer, body, next_ms, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![inbox, signer, body.to_string(), now_ms],
    )?;
    Ok(true)
}

/// The inboxes to reach a local actor's followers and a few named actors,
/// one per server where the server has a shared inbox.
pub fn inboxes(
    conn: &Connection,
    local: Option<&str>,
    also: &[String],
) -> rusqlite::Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    if let Some(local) = local {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT COALESCE(a.shared_inbox, a.inbox)
             FROM fedi_followers f JOIN fedi_actors a ON a.id = f.actor
             WHERE f.local = ?1",
        )?;
        for inbox in stmt.query_map(params![local], |r| r.get::<_, String>(0))? {
            out.push(inbox?);
        }
    }
    for actor in also {
        let inbox: Option<String> = conn
            .query_row(
                "SELECT COALESCE(shared_inbox, inbox) FROM fedi_actors WHERE id = ?1",
                params![actor],
                |r| r.get(0),
            )
            .ok();
        out.extend(inbox);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Queue `body` for every inbox in `inboxes`.
pub fn fan_out(
    conn: &Connection,
    inboxes: &[String],
    signer: &str,
    body: &Value,
    now_ms: i64,
) -> rusqlite::Result<usize> {
    let mut n = 0;
    for inbox in inboxes {
        if enqueue(conn, inbox, signer, body, now_ms)? {
            n += 1;
        }
    }
    Ok(n)
}

/// How long to wait before the `attempt`th retry.
#[must_use]
pub fn backoff(attempt: i64) -> Duration {
    let minutes = 1u64 << attempt.clamp(0, 9);
    Duration::from_secs((minutes * 60).min(6 * 3600))
}

struct Due {
    id: i64,
    inbox: String,
    signer: String,
    body: String,
    attempts: i64,
}

enum Outcome {
    Sent,
    /// The other server refused it for good (a 4xx other than a timeout or a
    /// rate limit): retrying would be refused the same way.
    Refused(String),
    Retry(String),
}

/// Send what is due until the server stops. Runs on the tokio runtime; the
/// requests themselves are blocking and go to the blocking pool.
pub async fn run(conn: Arc<Mutex<Connection>>, fed: Federation) {
    let client: Client = fed.client.clone();
    loop {
        let now = super::now_ms();
        let due: Vec<Due> = {
            let Ok(c) = conn.lock() else { return };
            let mut stmt = match c.prepare(
                "SELECT id, inbox, signer, body, attempts FROM fedi_deliveries
                 WHERE next_ms <= ?1 ORDER BY next_ms LIMIT ?2",
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("fediverse delivery: {e}");
                    return;
                }
            };
            stmt.query_map(params![now, BATCH as i64], |r| {
                Ok(Due {
                    id: r.get(0)?,
                    inbox: r.get(1)?,
                    signer: r.get(2)?,
                    body: r.get(3)?,
                    attempts: r.get(4)?,
                })
            })
            .and_then(Iterator::collect)
            .unwrap_or_default()
        };
        if due.is_empty() {
            let _ = tokio::time::timeout(IDLE, fed.wake.notified()).await;
            continue;
        }
        let mut tasks = Vec::with_capacity(due.len());
        for d in due {
            let key = {
                let Ok(c) = conn.lock() else { return };
                signing::private_key(&c, &d.signer, now)
            };
            let client = client.clone();
            let key_id = signer_url(&fed.base, &d.signer).map(|u| super::key_id(&u));
            tasks.push(tokio::task::spawn_blocking(move || {
                let outcome = match (key, key_id) {
                    (Ok(key), Some(key_id)) => {
                        match client.post_activity(&d.inbox, d.body.as_bytes(), &key, &key_id) {
                            Ok(_) => Outcome::Sent,
                            Err(e) => match e.status() {
                                Some(s) if (400..500).contains(&s) && s != 408 && s != 429 => {
                                    Outcome::Refused(e.to_string())
                                }
                                _ => Outcome::Retry(e.to_string()),
                            },
                        }
                    }
                    _ => Outcome::Refused(format!("no key for {}", d.signer)),
                };
                (d.id, d.inbox, d.attempts, outcome)
            }));
        }
        for task in tasks {
            let Ok((id, inbox, attempts, outcome)) = task.await else {
                continue;
            };
            let Ok(c) = conn.lock() else { return };
            let done = match outcome {
                Outcome::Sent => {
                    tracing::debug!(%inbox, "fediverse: delivered");
                    true
                }
                Outcome::Refused(why) => {
                    tracing::info!(%inbox, %why, "fediverse: delivery refused, dropped");
                    true
                }
                Outcome::Retry(why) if attempts + 1 >= MAX_ATTEMPTS => {
                    tracing::warn!(%inbox, %why, "fediverse: delivery given up");
                    true
                }
                Outcome::Retry(why) => {
                    let wait = i64::try_from(backoff(attempts).as_millis()).unwrap_or(i64::MAX);
                    tracing::info!(%inbox, %why, attempt = attempts + 1, "fediverse: delivery failed, will retry");
                    let _ = c.execute(
                        "UPDATE fedi_deliveries SET attempts = attempts + 1, next_ms = ?2 WHERE id = ?1",
                        params![id, super::now_ms().saturating_add(wait)],
                    );
                    false
                }
            };
            if done {
                let _ = c.execute("DELETE FROM fedi_deliveries WHERE id = ?1", params![id]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_wait_longer_each_time_and_never_more_than_six_hours() {
        assert_eq!(backoff(0), Duration::from_secs(60));
        assert_eq!(backoff(1), Duration::from_secs(120));
        assert_eq!(backoff(30), Duration::from_secs(6 * 3600));
        let total: Duration = (0..MAX_ATTEMPTS).map(backoff).sum();
        assert!(total > Duration::from_secs(2 * 86_400), "{total:?}");
    }

    #[test]
    fn nothing_is_queued_for_a_blocked_server_and_one_inbox_serves_a_server() {
        let conn = crate::db::open_and_migrate(":memory:").unwrap();
        blocklist::block(&conn, "spam.example", "", 1).unwrap();
        let body = serde_json::json!({"type": "Create"});
        assert!(!enqueue(&conn, "https://spam.example/inbox", "instance", &body, 1).unwrap());
        assert!(enqueue(&conn, "https://ok.example/inbox", "instance", &body, 1).unwrap());
        for (id, shared) in [
            ("https://m.test/users/a", Some("https://m.test/inbox")),
            ("https://m.test/users/b", Some("https://m.test/inbox")),
            ("https://l.test/u/c", None),
        ] {
            conn.execute(
                "INSERT INTO fedi_actors (id, kind, username, host, name, inbox, shared_inbox, key_id, public_pem, fetched_ms)
                 VALUES (?1, 'Person', 'x', 'h', 'x', ?1 || '/inbox', ?2, 'k', 'p', 0)",
                params![id, shared],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO fedi_followers (local, actor, followed_ms) VALUES ('c:x', ?1, 0)",
                params![id],
            )
            .unwrap();
        }
        assert_eq!(
            inboxes(&conn, Some("c:x"), &[]).unwrap(),
            ["https://l.test/u/c/inbox", "https://m.test/inbox"]
        );
    }
}
