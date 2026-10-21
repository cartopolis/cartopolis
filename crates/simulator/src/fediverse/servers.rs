//! What software another server runs, and what follows from it.
//!
//! The fediverse agrees on a core and differs at the edges, and two edges
//! matter here. **A community's boost**: Lemmy and the servers built like it
//! read a group's `Announce` of the `Create` activity (FEP-1b12) and refuse a
//! bare object id, which they try to read as an activity; Mastodon and its
//! kind read a boost of the object and ignore one of an activity. Sending both
//! to everybody made every boost an error on one side, so each server gets the
//! one it reads, and a server that never said what it runs gets both. **A
//! follow from somebody's own server**: each kind has its own page for it
//! (`follow_link`).
//!
//! The software is read from the server's NodeInfo and kept in `fedi_servers`,
//! for a week when it answered and a day when it did not.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use super::fetch::{Client, FetchError, MAX_JSON_BYTES};

/// How long a server's answer is believed.
const KNOWN_MS: i64 = 7 * 24 * 3600 * 1000;
/// How long before a server that did not answer is asked again.
const UNKNOWN_MS: i64 = 24 * 3600 * 1000;

/// Which boost a server reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Lemmy, Mbin, PieFed, …: communities with threads, which read the
    /// group's `Announce` of the activity.
    Threads,
    /// Mastodon, Pleroma, Misskey, GoToSocial, …: timelines, which read a
    /// boost of the object.
    Timelines,
    /// It never said.
    Unknown,
}

/// The family of a software name as NodeInfo gives it.
#[must_use]
pub fn family_of(software: &str) -> Family {
    match software.to_ascii_lowercase().as_str() {
        "lemmy" | "mbin" | "kbin" | "piefed" | "sublinks" | "nodebb" | "discourse" => {
            Family::Threads
        }
        "" => Family::Unknown,
        // Cartopolis reads both, and everything else is a timeline of some
        // kind: Mastodon, Pleroma, Akkoma, Misskey and its forks, GoToSocial,
        // Friendica, Pixelfed, …
        "cartopolis" => Family::Unknown,
        _ => Family::Timelines,
    }
}

/// The family of the server behind an inbox, from what is stored.
pub fn family(conn: &Connection, host: &str) -> rusqlite::Result<Family> {
    let software: Option<Option<String>> = conn
        .query_row(
            "SELECT software FROM fedi_servers WHERE host = ?1",
            params![host],
            |r| r.get(0),
        )
        .optional()?;
    Ok(software
        .flatten()
        .map_or(Family::Unknown, |s| family_of(&s)))
}

/// What a server runs, from the cache while it is fresh and from its NodeInfo
/// otherwise. `None` when it does not say.
pub fn software(
    conn: &std::sync::Mutex<Connection>,
    client: &Client,
    host: &str,
    now_ms: i64,
) -> Option<String> {
    let cached: Option<(Option<String>, i64)> = conn
        .lock()
        .ok()?
        .query_row(
            "SELECT software, checked_ms FROM fedi_servers WHERE host = ?1",
            params![host],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()?;
    if let Some((software, at)) = cached {
        let fresh = if software.is_some() {
            KNOWN_MS
        } else {
            UNKNOWN_MS
        };
        if now_ms - at < fresh {
            return software;
        }
    }
    let software = match read_nodeinfo(client, host) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(%host, "fediverse: no NodeInfo: {e}");
            None
        }
    };
    if let Ok(c) = conn.lock() {
        let _ = c.execute(
            "INSERT INTO fedi_servers (host, software, checked_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT (host) DO UPDATE SET software = ?2, checked_ms = ?3",
            params![host, software, now_ms],
        );
    }
    software
}

/// The software name a server's NodeInfo gives, through the discovery
/// document every NodeInfo server has at `/.well-known/nodeinfo`.
fn read_nodeinfo(client: &Client, host: &str) -> Result<Option<String>, FetchError> {
    let scheme = if client.allows_private() && host.contains(':') {
        "http"
    } else {
        "https"
    };
    let (bytes, from) = client.get(
        &format!("{scheme}://{host}/.well-known/nodeinfo"),
        "application/json",
        None,
        MAX_JSON_BYTES,
    )?;
    let links: Value = serde_json::from_slice(&bytes)
        .map_err(|e| FetchError::Unreadable(from.clone(), e.to_string()))?;
    // The newest schema the server offers; they all name the software alike.
    let Some(href) = links["links"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| {
            l["rel"]
                .as_str()
                .is_some_and(|r| r.starts_with("http://nodeinfo.diaspora.software/ns/schema/"))
        })
        .filter_map(|l| l["href"].as_str())
        .max()
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let (bytes, from) = client.get(&href, "application/json", None, MAX_JSON_BYTES)?;
    let info: Value =
        serde_json::from_slice(&bytes).map_err(|e| FetchError::Unreadable(from, e.to_string()))?;
    Ok(info["software"]["name"]
        .as_str()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty()))
}

/// The page on somebody's own server (`server`, running `software` if it said)
/// where they follow `address` (`handle@cartopolis.org`, no leading `@`). A
/// community on Lemmy is searched for with a `!`.
#[must_use]
pub fn follow_link(server: &str, software: Option<&str>, address: &str, group: bool) -> String {
    let at = format!("@{address}");
    let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    match software.unwrap_or("") {
        "lemmy" => {
            let (q, kind) = if group {
                (format!("!{address}"), "Communities")
            } else {
                (at, "Users")
            };
            format!("https://{server}/search?q={}&type={kind}", enc(&q))
        }
        "mbin" | "kbin" | "piefed" => {
            let q = if group { format!("!{address}") } else { at };
            format!("https://{server}/search?q={}", enc(&q))
        }
        "misskey" | "sharkey" | "firefish" | "iceshrimp" | "cherrypick" | "catodon" => {
            format!("https://{server}/authorize-follow?acct={}", enc(&at))
        }
        "pleroma" | "akkoma" => {
            format!("https://{server}/ostatus_subscribe?acct={}", enc(address))
        }
        "friendica" => format!("https://{server}/contact/follow?url={}", enc(&at)),
        // Mastodon and what copies its interface (Hometown, glitch-soc,
        // GoToSocial's Mastodon clients), and the guess for a server that did
        // not say.
        _ => format!("https://{server}/authorize_interaction?uri={}", enc(&at)),
    }
}

/// The server in what somebody typed as theirs: `mastodon.social`,
/// `@me@mastodon.social`, or `https://mastodon.social/`.
#[must_use]
pub fn server_in(typed: &str) -> Option<String> {
    let t = typed.trim();
    let t = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
        .unwrap_or(t);
    let t = t.split('/').next().unwrap_or(t);
    let host = t.rsplit_once('@').map_or(t, |(_, h)| h);
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let ok = host.contains('.')
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    ok.then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_server_reads_its_own_boost() {
        assert_eq!(family_of("lemmy"), Family::Threads);
        assert_eq!(family_of("PieFed"), Family::Threads);
        assert_eq!(family_of("mastodon"), Family::Timelines);
        assert_eq!(family_of("gotosocial"), Family::Timelines);
        assert_eq!(family_of("cartopolis"), Family::Unknown);
        assert_eq!(family_of(""), Family::Unknown);
    }

    #[test]
    fn a_follow_goes_to_the_page_each_server_has_for_it() {
        assert_eq!(
            follow_link(
                "mastodon.social",
                Some("mastodon"),
                "oost@cartopolis.org",
                true
            ),
            "https://mastodon.social/authorize_interaction?uri=%40oost%40cartopolis.org"
        );
        assert_eq!(
            follow_link("lemmy.world", Some("lemmy"), "oost@cartopolis.org", true),
            "https://lemmy.world/search?q=%21oost%40cartopolis.org&type=Communities"
        );
        assert_eq!(
            follow_link("misskey.io", Some("misskey"), "sanne@cartopolis.org", false),
            "https://misskey.io/authorize-follow?acct=%40sanne%40cartopolis.org"
        );
        assert!(follow_link("x.test", None, "a@b.test", false).contains("authorize_interaction"));
    }

    #[test]
    fn a_server_is_read_out_of_whatever_was_typed() {
        for typed in [
            "mastodon.social",
            " @me@mastodon.social ",
            "https://mastodon.social/",
            "Mastodon.Social.",
        ] {
            assert_eq!(
                server_in(typed).as_deref(),
                Some("mastodon.social"),
                "{typed}"
            );
        }
        assert_eq!(server_in("localhost"), None);
        assert_eq!(server_in("a b.test"), None);
        assert_eq!(server_in(""), None);
    }

    #[test]
    fn a_stored_answer_decides_the_family() {
        let conn = crate::db::open_and_migrate(":memory:").unwrap();
        conn.execute(
            "INSERT INTO fedi_servers (host, software, checked_ms) VALUES ('l.test', 'lemmy', 0),
                 ('q.test', NULL, 0)",
            [],
        )
        .unwrap();
        assert_eq!(family(&conn, "l.test").unwrap(), Family::Threads);
        assert_eq!(family(&conn, "q.test").unwrap(), Family::Unknown);
        assert_eq!(family(&conn, "new.test").unwrap(), Family::Unknown);
    }
}
