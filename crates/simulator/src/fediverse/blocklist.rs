//! The servers nobody here hears from or sends to.
//!
//! Federation is open — there is no list of servers to talk to — so the
//! control is the other way round: a list of servers to refuse. It starts from
//! a **shared block list** maintained by people who moderate fediverse servers
//! for a living (`SimConfig::fediverse_blocklists`; Garden Fence by default,
//! chosen on 2026-10-13 for being curated with reasons per entry and updated
//! that week, where Oliphant's tier-0 list had last changed in April), re-read
//! once a day, and the operator adds their own entries on top. The two never
//! mix: an import replaces only the rows it wrote (`source = 'shared'`), so a
//! server the operator blocked stays blocked when the shared list drops it.
//!
//! A block covers the server and every subdomain of it, and applies both ways:
//! nothing from it is accepted and nothing is delivered to it.

use rusqlite::{params, Connection, OptionalExtension};

/// The list used when the operator names none: Garden Fence, in the CSV form
/// Mastodon imports.
pub const DEFAULT_LIST: &str =
    "https://raw.githubusercontent.com/gardenfence/blocklist/main/gardenfence-mastodon.csv";

/// How often the shared list is read again.
pub const REFRESH: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// The largest list read: Garden Fence is ~10 kB and Oliphant's ~30 kB.
pub const MAX_LIST_BYTES: u64 = 2 << 20;

/// One entry: a server and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub host: String,
    pub reason: String,
}

/// Read a block list: Mastodon's CSV export (`#domain,#severity,…`, with or
/// without the `#`s), or one server per line. Only `suspend` entries count —
/// `silence` means "don't show in public timelines", which has no equivalent
/// here and is not a reason to refuse a reply. Obfuscated entries (`*` in the
/// name) are skipped, since there is no way to know which server they mean.
#[must_use]
pub fn parse(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut header: Option<Vec<String>> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields = csv_fields(line);
        if header.is_none()
            && fields.first().is_some_and(|f| {
                let f = f.trim_start_matches('#');
                f == "domain"
            })
        {
            header = Some(
                fields
                    .iter()
                    .map(|f| f.trim_start_matches('#').to_ascii_lowercase())
                    .collect(),
            );
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let col = |name: &str| -> Option<&str> {
            let i = header.as_ref()?.iter().position(|h| h == name)?;
            fields.get(i).map(String::as_str)
        };
        let host = fields
            .first()
            .map(|h| h.trim().trim_end_matches('.').to_ascii_lowercase())
            .unwrap_or_default();
        if host.is_empty() || host.contains('*') || host.contains(['/', ' ', '@']) {
            continue;
        }
        if let Some(severity) = col("severity") {
            if !severity.trim().eq_ignore_ascii_case("suspend") {
                continue;
            }
        }
        out.push(Entry {
            reason: col("public_comment").unwrap_or_default().trim().to_string(),
            host,
        });
    }
    out
}

/// The fields of one CSV line, quotes honoured.
fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut field)),
            c => field.push(c),
        }
    }
    out.push(field);
    out
}

/// Replace the imported entries with a fresh list. The operator's own entries
/// are untouched. Answers how many entries the list now holds.
pub fn replace_shared(
    conn: &Connection,
    entries: &[Entry],
    now_ms: i64,
) -> rusqlite::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM fedi_server_blocks WHERE source = 'shared'", [])?;
    let mut n = 0;
    for e in entries {
        n += tx.execute(
            "INSERT OR IGNORE INTO fedi_server_blocks (host, source, reason, added_ms)
             VALUES (?1, 'shared', ?2, ?3)",
            params![e.host, e.reason, now_ms],
        )?;
    }
    tx.commit()?;
    Ok(n)
}

/// The operator blocks a server.
pub fn block(conn: &Connection, host: &str, reason: &str, now_ms: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO fedi_server_blocks (host, source, reason, added_ms) VALUES (?1, 'admin', ?2, ?3)
         ON CONFLICT (host) DO UPDATE SET source = 'admin', reason = ?2",
        params![host.trim().to_ascii_lowercase(), reason, now_ms],
    )?;
    Ok(())
}

/// The operator unblocks a server. An entry the shared list holds comes back
/// at its next import; that is the operator's to take up with the list.
pub fn unblock(conn: &Connection, host: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM fedi_server_blocks WHERE host = ?1",
        params![host.trim().to_ascii_lowercase()],
    )
}

/// Whether `host` or any domain above it is blocked.
pub fn is_blocked(conn: &Connection, host: &str) -> rusqlite::Result<bool> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let mut candidate = host.as_str();
    loop {
        let hit = conn
            .query_row(
                "SELECT 1 FROM fedi_server_blocks WHERE host = ?1",
                params![candidate],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if hit {
            return Ok(true);
        }
        match candidate.split_once('.') {
            Some((_, parent)) if parent.contains('.') => candidate = parent,
            _ => return Ok(false),
        }
    }
}

/// Every block, the operator's first.
pub fn list(conn: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT host, source, reason FROM fedi_server_blocks
         ORDER BY source = 'shared', host",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mastodons_csv_reads_and_only_suspensions_count() {
        let text = "#domain,#severity,#reject_media,#reject_reports,#public_comment,#obfuscate
spam.example,suspend,false,false,\"spam, harassment\",false
quiet.example,silence,false,false,,false
s*m.example,suspend,false,false,,true
Bad.Example.,suspend,false,false,hate,false
";
        let e = parse(text);
        assert_eq!(
            e,
            vec![
                Entry {
                    host: "spam.example".into(),
                    reason: "spam, harassment".into()
                },
                Entry {
                    host: "bad.example".into(),
                    reason: "hate".into()
                },
            ]
        );
    }

    #[test]
    fn a_plain_list_and_a_header_without_hashes_read_too() {
        assert_eq!(
            parse("# comment\none.example\ntwo.example\n")
                .into_iter()
                .map(|e| e.host)
                .collect::<Vec<_>>(),
            ["one.example", "two.example"]
        );
        assert_eq!(
            parse("domain,severity\nx.example,suspend\n")
                .into_iter()
                .map(|e| e.host)
                .collect::<Vec<_>>(),
            ["x.example"]
        );
    }

    #[test]
    fn a_block_covers_subdomains_and_an_import_spares_the_operators_own() {
        let conn = crate::db::open_and_migrate(":memory:").unwrap();
        block(&conn, "Mine.example", "ours", 1).unwrap();
        replace_shared(
            &conn,
            &[Entry {
                host: "spam.example".into(),
                reason: String::new(),
            }],
            2,
        )
        .unwrap();
        assert!(is_blocked(&conn, "spam.example").unwrap());
        assert!(is_blocked(&conn, "media.spam.example").unwrap());
        assert!(!is_blocked(&conn, "notspam.example").unwrap());
        assert!(!is_blocked(&conn, "example").unwrap());
        replace_shared(&conn, &[], 3).unwrap();
        assert!(!is_blocked(&conn, "spam.example").unwrap());
        assert!(is_blocked(&conn, "mine.example").unwrap());
        assert_eq!(list(&conn).unwrap()[0].1, "admin");
    }
}
