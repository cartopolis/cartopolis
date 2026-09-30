//! Waking a phone whose app is closed when somebody calls (protocol 29,
//! `docs/notes/calls.md`).
//!
//! A connected device hears a call ring through `CallRoster`. A closed app
//! hears nothing, so each device may register a **UnifiedPush** endpoint
//! (`SetPushEndpoint`): an `https:` URL its distributor app (ntfy, say) gave
//! it. When a call *starts* in a chat, this posts to the endpoints of every
//! other member, and the app's receiver shows "Incoming call"; opening it
//! connects, and the roster rings as it always has.
//!
//! **The push carries nothing.** Not who is calling, not which chat — the
//! distributor's server relays it and has no business knowing. A wake-up is
//! all the phone needs, because the app learns the rest over its own
//! connection. (UnifiedPush over Firebase is not used and would not change
//! that: the push is a wake-up whichever way it travels.)
//!
//! **The address is a stranger's choice, and this server sits inside a
//! private network** — nominatim, overpass and Forgejo are one hop away. So a
//! push goes only to `https:` and only to a public address, and the check is
//! made at the moment of connecting (a resolver that returns public addresses
//! only), not by looking the name up beforehand, which a name that resolves
//! differently the second time would walk straight past.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::db::{self, SharedConn};

/// Longest endpoint accepted. A UnifiedPush endpoint is a URL with a random
/// token in it; anything longer than this is not one.
pub const MAX_ENDPOINT_LEN: usize = 1024;

/// One push per endpoint per this long: a call that is started, dropped and
/// started again rings once, not three times.
const QUIET: Duration = Duration::from_secs(20);

/// How long a distributor gets to take the push.
const TIMEOUT: Duration = Duration::from_secs(8);

/// A call has started in `group`: wake everybody else in it.
#[derive(Debug, Clone)]
pub struct Ring {
    /// `accounts.id` of the person who started it — their own devices are
    /// not rung.
    pub caller: String,
    /// Their username, for the block check.
    pub caller_name: String,
    /// Every member account of the chat (`accounts.id`).
    pub members: Vec<String>,
}

/// Why an endpoint is refused, in words for the log.
pub fn check_endpoint(endpoint: &str) -> Result<(), &'static str> {
    if endpoint.len() > MAX_ENDPOINT_LEN {
        return Err("too long");
    }
    let Some(rest) = endpoint.strip_prefix("https://") else {
        return Err("not https");
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("no host, or credentials in it");
    }
    if endpoint
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("not a URL");
    }
    Ok(())
}

/// Whether an address is one this server may send a stranger's push to.
fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || a == 0
                || (a == 100 && (64..128).contains(&b)) // shared address space
                || (a == 198 && (b == 18 || b == 19)) // benchmarking
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // unique local
                || (first & 0xffc0) == 0xfe80) // link-local
        }
    }
}

/// Resolves names as usual and keeps only the public addresses: what the
/// connection is made to is what was checked.
struct PublicOnly;

impl ureq::Resolver for PublicOnly {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        let addrs: Vec<SocketAddr> = netloc
            .to_socket_addrs()?
            .filter(|a| public(a.ip()))
            .collect();
        if addrs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "push endpoint does not resolve to a public address",
            ));
        }
        Ok(addrs)
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .resolver(PublicOnly)
        .timeout(TIMEOUT)
        .redirects(0)
        .user_agent(concat!("cartopolis-simulator/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// What became of one push.
#[derive(Debug, PartialEq, Eq)]
enum Sent {
    Taken,
    /// The distributor says the endpoint no longer exists.
    Gone,
    Failed,
}

fn post(agent: &ureq::Agent, endpoint: &str) -> Sent {
    // RFC 8030's headers, which UnifiedPush distributors honour: a call that
    // cannot be delivered within a minute is not worth delivering at all.
    let result = agent
        .post(endpoint)
        .set("TTL", "60")
        .set("Urgency", "high")
        .set("Content-Type", "application/octet-stream")
        .send_bytes(b"1");
    match result {
        Ok(_) => Sent::Taken,
        Err(ureq::Error::Status(404 | 410, _)) => Sent::Gone,
        Err(e) => {
            tracing::debug!("push not taken: {e}");
            Sent::Failed
        }
    }
}

/// The endpoints a call start wakes: every member's but the caller's own, and
/// nobody's who blocked the caller — the roster does not ring them either.
fn endpoints_for(c: &rusqlite::Connection, ring: &Ring) -> Vec<String> {
    let mut out = Vec::new();
    for account in ring.members.iter().filter(|a| **a != ring.caller) {
        let blocks = db::blocked_usernames(c, account).unwrap_or_default();
        if blocks
            .iter()
            .any(|b| b.eq_ignore_ascii_case(&ring.caller_name))
        {
            continue;
        }
        out.extend(db::push_endpoints(c, account).unwrap_or_default());
    }
    out
}

/// Start the task that sends the pushes; rings go in through the channel.
pub fn spawn(conn: SharedConn) -> mpsc::UnboundedSender<Ring> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(conn, rx));
    tx
}

async fn run(conn: SharedConn, mut rx: mpsc::UnboundedReceiver<Ring>) {
    let mut last: HashMap<String, Instant> = HashMap::new();
    while let Some(ring) = rx.recv().await {
        let found = db::call(&conn, move |c| endpoints_for(c, &ring)).await;
        let Ok(endpoints) = found else { continue };
        let now = Instant::now();
        last.retain(|_, at| now.duration_since(*at) < QUIET);
        let due: Vec<String> = endpoints
            .into_iter()
            .filter(|e| !last.contains_key(e) && check_endpoint(e).is_ok())
            .collect();
        for e in &due {
            last.insert(e.clone(), now);
        }
        if due.is_empty() {
            continue;
        }
        let results = tokio::task::spawn_blocking(move || {
            let agent = agent();
            due.into_iter()
                .map(|e| {
                    let sent = post(&agent, &e);
                    (e, sent)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let taken = results.iter().filter(|(_, s)| *s == Sent::Taken).count();
        tracing::info!(taken, of = results.len(), "rang closed apps for a call");
        for (endpoint, sent) in results {
            if sent == Sent::Gone {
                let _ = db::call(&conn, move |c| db::drop_push_endpoint(c, &endpoint)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_public_https_address_is_an_endpoint() {
        assert!(check_endpoint("https://ntfy.sh/upAbC123?up=1").is_ok());
        assert!(check_endpoint("http://ntfy.sh/up").is_err());
        assert!(check_endpoint("https://user:pw@ntfy.sh/up").is_err());
        assert!(check_endpoint("https:///up").is_err());
        assert!(check_endpoint(&format!("https://x/{}", "a".repeat(MAX_ENDPOINT_LEN))).is_err());
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.17.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!public(private.parse().unwrap()), "{private}");
        }
        for open in ["51.195.91.181", "1.1.1.1", "2a01:4f8::1"] {
            assert!(public(open.parse().unwrap()), "{open}");
        }
    }

    #[test]
    fn a_call_wakes_the_others_but_not_the_caller_or_who_blocked_them() {
        let dir = tempfile::tempdir().unwrap();
        let c = db::open_and_migrate(dir.path().join("w.db").to_str().unwrap()).unwrap();
        for (account, endpoint) in [
            ("ann", "https://push.example/ann-phone"),
            ("ann", "https://push.example/ann-tablet"),
            ("bob", "https://push.example/bob"),
            ("cy", "https://push.example/cy"),
        ] {
            db::set_push_endpoint(&c, account, endpoint, 1).unwrap();
        }
        db::set_block(&c, "cy", "Ann", true).unwrap();
        let ring = Ring {
            caller: "ann".into(),
            caller_name: "ann".into(),
            members: vec!["ann".into(), "bob".into(), "cy".into()],
        };
        assert_eq!(endpoints_for(&c, &ring), vec!["https://push.example/bob"]);

        // A device that signs in as somebody else takes its endpoint along,
        // and a device can only take back its own.
        db::set_push_endpoint(&c, "cy", "https://push.example/bob", 2).unwrap();
        assert!(db::push_endpoints(&c, "bob").unwrap().is_empty());
        db::remove_push_endpoint(&c, "bob", "https://push.example/cy").unwrap();
        assert_eq!(db::push_endpoints(&c, "cy").unwrap().len(), 2);
    }

    #[test]
    fn the_resolver_refuses_a_name_for_a_private_address() {
        let err = ureq::Resolver::resolve(&PublicOnly, "127.0.0.1:443").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let err = ureq::Resolver::resolve(&PublicOnly, "localhost:443").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }
}
