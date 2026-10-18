//! `stats.db`: opening it, the once-a-minute write, and closing a day.

use super::{day_of, SessionOp, KEEP_DAYS, MIN_PEOPLE};
use anyhow::Context;
use rusqlite::{params, Connection};
use uuid::Uuid;

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations_stats");
}

/// Open and migrate, then repair what a stopped server left: sessions it never
/// saw end, and days it never closed.
pub(super) fn open(path: &str, now: i64) -> anyhow::Result<Connection> {
    if path != ":memory:" {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all {parent:?}"))?;
        }
    }
    let mut conn = Connection::open(path).with_context(|| format!("open sqlite {path}"))?;
    // The world's reasons apply unchanged (`crate::db::tune`): one writer, WAL,
    // and losing the last minute to a power cut costs a minute of counts.
    let _: Result<String, _> = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0));
    let _ = conn.pragma_update(None, "synchronous", "NORMAL");
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    embedded::migrations::runner()
        .run(&mut conn)
        .context("stats migrate")?;
    conn.execute(
        "UPDATE sessions SET ended = last_seen WHERE ended IS NULL",
        [],
    )?;
    let today = day_of(now);
    // Close every day a stopped server missed, oldest first.
    let first: Option<i64> =
        conn.query_row("SELECT MIN(started) / 86400 FROM sessions", [], |r| {
            r.get(0)
        })?;
    let start = first.unwrap_or(today).max(today - KEEP_DAYS);
    for day in start..today {
        let done: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM active_daily WHERE day = ?1 AND client = 'all')",
            [day],
            |r| r.get(0),
        )?;
        if !done {
            close_day(&conn, day, now)?;
        }
    }
    conn.execute(
        "DELETE FROM squares_daily WHERE day < ?1 AND people < ?2",
        params![today, MIN_PEOPLE],
    )?;
    Ok(conn)
}

pub(super) struct WebRow {
    pub site: &'static str,
    pub dim: &'static str,
    pub value: String,
    pub visitors: i64,
    pub views: i64,
}

pub(super) struct RefusedRow {
    pub reason: &'static str,
    pub protocol: u16,
    pub client: &'static str,
    pub build: String,
    pub attempts: i64,
    pub devices: i64,
}

pub(super) struct SquareRow {
    pub sx: i64,
    pub sy: i64,
    pub minutes: i64,
    pub people: i64,
}

/// Everything counted since the last write, for the day it was counted on.
#[derive(Default)]
pub(super) struct Batch {
    pub day: i64,
    /// The day to close after writing, when the date has moved on.
    pub closes: Option<i64>,
    pub sessions: Vec<SessionOp>,
    pub web: Vec<WebRow>,
    pub refused: Vec<RefusedRow>,
    pub events: Vec<((&'static str, String, String), i64)>,
    pub activity: Vec<(Uuid, String, String)>,
    pub squares: Vec<SquareRow>,
    pub online: Option<(i64, i64, i64, i64)>,
}

pub(super) fn write(c: &mut Connection, b: &Batch, now: i64) -> anyhow::Result<()> {
    let tx = c.transaction()?;
    for op in &b.sessions {
        match op {
            SessionOp::Start {
                id,
                account,
                client,
                build,
                protocol,
                at,
            } => {
                tx.execute(
                    "INSERT OR IGNORE INTO sessions (id, account, client, build, protocol, started, last_seen) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![id.to_string(), account.to_string(), client, build, protocol, at],
                )?;
            }
            SessionOp::End { id, at } => {
                tx.execute(
                    "UPDATE sessions SET ended = ?2, last_seen = ?2 WHERE id = ?1",
                    params![id.to_string(), at],
                )?;
            }
        }
    }
    tx.execute(
        "UPDATE sessions SET last_seen = ?1 WHERE ended IS NULL",
        [now],
    )?;
    // Visitors are a running distinct count for the day, so the larger of the
    // stored and the counted wins (a restart starts the count over); views and
    // minutes are deltas.
    for r in &b.web {
        tx.execute(
            "INSERT INTO web_daily (day, site, dim, value, visitors, views) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (day, site, dim, value) DO UPDATE SET \
             visitors = MAX(visitors, excluded.visitors), views = views + excluded.views",
            params![b.day, r.site, r.dim, r.value, r.visitors, r.views],
        )?;
    }
    for r in &b.refused {
        tx.execute(
            "INSERT INTO refused_daily (day, reason, protocol, client, build, attempts, devices) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (day, reason, protocol, client, build) DO UPDATE SET \
             attempts = attempts + excluded.attempts, devices = MAX(devices, excluded.devices)",
            params![b.day, r.reason, r.protocol, r.client, r.build, r.attempts, r.devices],
        )?;
    }
    for ((kind, community, topic), n) in &b.events {
        tx.execute(
            "INSERT INTO events_daily (day, kind, community, topic, n) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (day, kind, community, topic) DO UPDATE SET n = n + excluded.n",
            params![b.day, kind, community, topic, n],
        )?;
    }
    for (account, community, topic) in &b.activity {
        tx.execute(
            "INSERT OR IGNORE INTO activity (day, account, community, topic) VALUES (?1, ?2, ?3, ?4)",
            params![b.day, account.to_string(), community, topic],
        )?;
    }
    for r in &b.squares {
        tx.execute(
            "INSERT INTO squares_daily (day, sx, sy, minutes, people) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (day, sx, sy) DO UPDATE SET \
             minutes = minutes + excluded.minutes, people = MAX(people, excluded.people)",
            params![b.day, r.sx, r.sy, r.minutes, r.people],
        )?;
    }
    if let Some((hour, sum, samples, peak)) = b.online {
        tx.execute(
            "INSERT INTO online_hourly (hour, sum, samples, peak) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (hour) DO UPDATE SET \
             sum = MAX(sum, excluded.sum), samples = MAX(samples, excluded.samples), \
             peak = MAX(peak, excluded.peak)",
            params![hour, sum, samples, peak],
        )?;
    }
    if let Some(day) = b.closes {
        close_day(&tx, day, now)?;
    }
    tx.commit()?;
    Ok(())
}

/// Fold a finished day into what is kept for good, and let go of what is not:
/// the day's different people per app, squares too few people stood in, and
/// per-account rows past [`KEEP_DAYS`].
fn close_day(c: &Connection, day: i64, now: i64) -> anyhow::Result<()> {
    let (from, to) = (day * 86_400, (day + 1) * 86_400);
    c.execute(
        "INSERT OR REPLACE INTO active_daily (day, client, people) \
         SELECT ?1, client, COUNT(DISTINCT account) FROM sessions \
         WHERE started < ?3 AND COALESCE(ended, last_seen) >= ?2 GROUP BY client",
        params![day, from, to],
    )?;
    c.execute(
        "INSERT OR REPLACE INTO active_daily (day, client, people) \
         SELECT ?1, 'all', COUNT(DISTINCT account) FROM sessions \
         WHERE started < ?3 AND COALESCE(ended, last_seen) >= ?2",
        params![day, from, to],
    )?;
    c.execute(
        "DELETE FROM squares_daily WHERE day <= ?1 AND people < ?2",
        params![day, MIN_PEOPLE],
    )?;
    let cutoff = day_of(now) - KEEP_DAYS;
    c.execute(
        "DELETE FROM sessions WHERE COALESCE(ended, last_seen) < ?1",
        [cutoff * 86_400],
    )?;
    c.execute("DELETE FROM activity WHERE day < ?1", [cutoff])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_day_keeps_counts_and_drops_thin_squares() {
        let now = 20_000 * 86_400 + 3600;
        let mut c = open(":memory:", now).unwrap();
        let day = 19_999;
        let b = Batch {
            day,
            closes: Some(day),
            sessions: (0..3)
                .map(|i| SessionOp::Start {
                    id: Uuid::new_v4(),
                    account: Uuid::from_u128(i % 2),
                    client: "android",
                    build: String::new(),
                    protocol: 41,
                    at: day * 86_400 + 100,
                })
                .collect(),
            squares: vec![
                SquareRow {
                    sx: 1,
                    sy: 1,
                    minutes: 30,
                    people: 7,
                },
                SquareRow {
                    sx: 2,
                    sy: 2,
                    minutes: 30,
                    people: 2,
                },
            ],
            ..Default::default()
        };
        write(&mut c, &b, now).unwrap();
        let people: i64 = c
            .query_row(
                "SELECT people FROM active_daily WHERE day=?1 AND client='all'",
                [day],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(people, 2);
        let squares: i64 = c
            .query_row("SELECT COUNT(*) FROM squares_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(squares, 1);
    }

    #[test]
    fn reopening_closes_dangling_sessions_and_missed_days() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.db");
        let path = path.to_str().unwrap();
        let day = 20_000;
        {
            let mut c = open(path, day * 86_400).unwrap();
            let b = Batch {
                day,
                sessions: vec![SessionOp::Start {
                    id: Uuid::new_v4(),
                    account: Uuid::new_v4(),
                    client: "web",
                    build: String::new(),
                    protocol: 41,
                    at: day * 86_400 + 60,
                }],
                ..Default::default()
            };
            write(&mut c, &b, day * 86_400 + 120).unwrap();
        }
        let c = open(path, (day + 2) * 86_400).unwrap();
        let open_sessions: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE ended IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(open_sessions, 0);
        let closed: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM active_daily WHERE client='all'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(closed, 2);
    }
}
