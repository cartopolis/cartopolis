//! Public community posts, and this server's part in the fediverse
//! (`docs/notes/fediverse.md`, `docs/mockups/communities/v4-public-posts.html`).
//!
//! A community may switch on a public half beside its encrypted topics: posts,
//! each with a place, that anybody may read. With them on, the community is an
//! ActivityPub **group** (`@handle@server`), as a Lemmy community is: people on
//! Mastodon, Lemmy and the rest follow it, reply to its posts and post to it by
//! mentioning it. A post goes out under its author's own address
//! (`@name@server`, an ActivityPub **person**) and the community boosts it, which
//! is how Lemmy communities work and what gives a reply somebody to answer. A
//! community can also follow accounts and communities elsewhere and show what
//! they post.
//!
//! Federation is open: nothing here lists the servers we talk to. Anything a
//! followed account, a follower or a reply brings is accepted unless its server
//! is on the block list (`blocklist`), which starts from a shared, maintained
//! list and adds the operator's own entries.
//!
//! What is deliberately *not* here: nothing private ever goes out — topics and
//! private chats are encrypted and stay on the wire — and nothing real-time
//! does either.
//!
//! The modules:
//!
//! - [`posts`]: the rules over the database, network-free and tested there.
//! - [`signing`]: keys and HTTP signatures.
//! - [`fetch`]: every request to another server, behind one guard against
//!   being turned on this server's own private network.
//! - [`objects`]: the ActivityStreams JSON this server writes and reads.
//! - [`inbox`]: what to do with an activity another server sends.
//! - [`delivery`]: the queue of activities waiting to go out, retried.
//! - [`blocklist`]: the servers nobody here hears from.
//! - [`routes`]: the HTTP surface, both the fediverse's and the apps'.

pub mod blocklist;
pub mod delivery;
pub mod fetch;
pub mod inbox;
pub mod objects;
pub mod posts;
pub mod routes;
pub mod signing;

use std::sync::Arc;
use uuid::Uuid;

use crate::config::SimConfig;

/// Who this server is on the fediverse, and whether it is on it at all.
#[derive(Clone)]
pub struct Federation {
    /// The public origin every id here starts with (`https://cartopolis.org`),
    /// without a trailing slash.
    pub base: String,
    /// The host of that origin: what follows the `@` in an address.
    pub server: String,
    /// Whether this server talks to other servers. Off for a server whose
    /// public address is not a public `https` origin: a development sim on
    /// `localhost` keeps its posts to itself, since nobody could fetch them.
    pub enabled: bool,
    /// Tests only: talk to `http` and to private addresses. Never set in a
    /// deployment; it is the switch that turns the request guard off.
    pub allow_private: bool,
    /// Wakes the delivery worker when something is queued.
    pub wake: Arc<tokio::sync::Notify>,
    /// Every request to another server goes through this.
    pub client: fetch::Client,
}

impl Federation {
    #[must_use]
    pub fn from_config(config: &SimConfig) -> Self {
        let base = config
            .public_base_url
            .trim()
            .trim_end_matches('/')
            .to_string();
        let server = config.server_name();
        let public_https = base.starts_with("https://") && server != "localhost";
        Self {
            enabled: config.fediverse && (public_https || config.fediverse_allow_private),
            allow_private: config.fediverse_allow_private,
            base,
            server,
            wake: Arc::new(tokio::sync::Notify::new()),
            client: fetch::Client::new(config.fediverse_allow_private),
        }
    }
}

/// Read the shared block lists now and once a day, for as long as the server
/// runs. A list that cannot be read leaves the last import standing rather
/// than emptying it.
pub async fn refresh_blocklists(
    conn: Arc<std::sync::Mutex<rusqlite::Connection>>,
    fed: Federation,
    lists: Vec<String>,
) {
    if lists.is_empty() {
        return;
    }
    loop {
        let client = fed.client.clone();
        let urls = lists.clone();
        let read = tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            for url in &urls {
                match client.get(url, "text/csv, text/plain", None, blocklist::MAX_LIST_BYTES) {
                    Ok((bytes, _)) => {
                        entries.extend(blocklist::parse(&String::from_utf8_lossy(&bytes)));
                    }
                    Err(e) => {
                        tracing::warn!(%url, "fediverse: block list not read: {e}");
                        return None;
                    }
                }
            }
            Some(entries)
        })
        .await
        .ok()
        .flatten();
        if let Some(entries) = read {
            match conn.lock() {
                Ok(c) => match blocklist::replace_shared(&c, &entries, now_ms()) {
                    Ok(n) => tracing::info!(servers = n, "fediverse: shared block list read"),
                    Err(e) => tracing::warn!("fediverse: block list not stored: {e}"),
                },
                Err(_) => return,
            }
        }
        tokio::time::sleep(blocklist::REFRESH).await;
    }
}

/// A post's or reply's fediverse id.
#[must_use]
pub fn post_url(base: &str, id: Uuid) -> String {
    format!("{base}/ap/p/{id}")
}

/// An account's actor.
#[must_use]
pub fn person_url(base: &str, account: &str) -> String {
    format!("{base}/ap/u/{account}")
}

/// A community's actor.
#[must_use]
pub fn group_url(base: &str, community: Uuid) -> String {
    format!("{base}/ap/c/{community}")
}

/// This server's own actor, which signs the requests nobody in particular makes
/// (fetching an account's profile, say).
#[must_use]
pub fn instance_url(base: &str) -> String {
    format!("{base}/ap/actor")
}

/// The id of the key an actor publishes.
#[must_use]
pub fn key_id(actor_url: &str) -> String {
    format!("{actor_url}#main-key")
}

/// The actor URL for a signer row in `fedi_keys` (`instance`, `u:…`, `c:…`).
#[must_use]
pub fn signer_url(base: &str, signer: &str) -> Option<String> {
    if signer == "instance" {
        return Some(instance_url(base));
    }
    if let Some(account) = signer.strip_prefix("u:") {
        return Some(person_url(base, account));
    }
    let community = Uuid::parse_str(signer.strip_prefix("c:")?).ok()?;
    Some(group_url(base, community))
}

/// What a local actor URL names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Local {
    Person(String),
    Group(Uuid),
    Instance,
}

/// Whether `url` is one of this server's actors, and which.
#[must_use]
pub fn local_actor(base: &str, url: &str) -> Option<Local> {
    let rest = url.strip_prefix(base)?.strip_prefix("/ap/")?;
    if rest == "actor" {
        return Some(Local::Instance);
    }
    if let Some(id) = rest.strip_prefix("u/") {
        let id = Uuid::parse_str(id).ok()?;
        return Some(Local::Person(id.to_string()));
    }
    let id = Uuid::parse_str(rest.strip_prefix("c/")?).ok()?;
    Some(Local::Group(id))
}

/// Whether `url` is one of this server's posts, and which.
#[must_use]
pub fn local_post(base: &str, url: &str) -> Option<Uuid> {
    Uuid::parse_str(url.strip_prefix(base)?.strip_prefix("/ap/p/")?).ok()
}

/// The link that opens the map at a post's view: the same `/go?place=` a place
/// link is (`docs/notes/place-links.md`), on this server's own origin.
#[must_use]
pub fn place_link(base: &str, place: &cartopolis_core::posts::PostPlace) -> String {
    let alt = place.view.map_or(250.0, |v| v.altitude);
    let look = place
        .view
        .map(|v| format!(",{:.1},{:.1}", v.pitch, v.yaw))
        .unwrap_or_default();
    format!(
        "{base}/go?place={:.5},{:.5},{alt:.0}m{look}",
        place.latitude, place.longitude
    )
}

/// The place a Cartopolis link names (`…/go?place=53.21940,6.56650,120m,-12.5,45.0`),
/// from whichever server: how a post from Mastodon, which has no place of its
/// own, gets one.
#[must_use]
pub fn place_from_link(url: &str) -> Option<cartopolis_core::posts::PostPlace> {
    let (_, query) = url.split('#').next()?.split_once('?')?;
    let value = query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| k.eq_ignore_ascii_case("place"))?
        .1
        .replace("%2C", ",")
        .replace("%2c", ",");
    let mut parts = value.split(',').map(str::trim);
    let latitude: f64 = parts.next()?.parse().ok()?;
    let longitude: f64 = parts.next()?.parse().ok()?;
    let altitude: Option<f64> = parts
        .next()
        .and_then(|a| a.trim_end_matches('m').parse().ok());
    let pitch: Option<f32> = parts.next().and_then(|p| p.parse().ok());
    let yaw: Option<f32> = parts.next().and_then(|p| p.parse().ok());
    let view = match (altitude, pitch, yaw) {
        (Some(altitude), Some(pitch), Some(yaw)) => Some(cartopolis_core::point::PointView {
            altitude,
            pitch,
            yaw,
        }),
        _ => None,
    };
    let place = cartopolis_core::posts::PostPlace {
        latitude,
        longitude,
        name: None,
        view,
    };
    place.in_range().then_some(place)
}

/// Milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::point::PointView;
    use cartopolis_core::posts::PostPlace;

    #[test]
    fn a_local_url_names_what_it_is() {
        let base = "https://c.test";
        let id = Uuid::new_v4();
        assert_eq!(
            local_actor(base, &group_url(base, id)),
            Some(Local::Group(id))
        );
        assert_eq!(
            local_actor(base, &person_url(base, &id.to_string())),
            Some(Local::Person(id.to_string()))
        );
        assert_eq!(
            local_actor(base, &instance_url(base)),
            Some(Local::Instance)
        );
        assert_eq!(local_actor(base, "https://other.test/ap/actor"), None);
        assert_eq!(local_post(base, &post_url(base, id)), Some(id));
        assert_eq!(
            signer_url(base, &format!("c:{id}")),
            Some(group_url(base, id))
        );
    }

    #[test]
    fn a_place_link_reads_back_into_the_place() {
        let place = PostPlace {
            latitude: 53.2194,
            longitude: 6.5665,
            name: None,
            view: Some(PointView {
                altitude: 120.0,
                pitch: -12.5,
                yaw: 45.0,
            }),
        };
        let link = place_link("https://cartopolis.org", &place);
        assert_eq!(
            link,
            "https://cartopolis.org/go?place=53.21940,6.56650,120m,-12.5,45.0"
        );
        assert_eq!(place_from_link(&link), Some(place));
        assert_eq!(
            place_from_link("https://x.test/go?place=53.2,6.5").map(|p| p.view),
            Some(None)
        );
        assert_eq!(place_from_link("https://x.test/go?place=95,6.5"), None);
        assert_eq!(place_from_link("https://x.test/@anna/1"), None);
    }
}
