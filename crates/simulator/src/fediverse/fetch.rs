//! Every request this server makes to another server, behind one guard.
//!
//! **Federation means fetching URLs that strangers chose.** An actor's inbox, a
//! post's id, the picture attached to it, the key that signed a request: each is
//! a URL somebody else wrote, and the simulator runs on a machine whose private
//! network also holds nominatim, overpass and Forgejo. So nothing here connects
//! anywhere until [`GuardedResolver`] has resolved the name and found only
//! public addresses, and only to `https` on the default port. That check runs
//! for every connection ureq makes, so a redirect or a second request to a name
//! that now resolves somewhere else is checked again; redirects are followed by
//! hand for the same reason, with the scheme checked each time.
//!
//! **Blocking.** Called from `spawn_blocking`; `ureq` is what the rest of this
//! workspace fetches with (`buildings`), and an async client would put a second
//! TLS stack in the tree.

use std::io::Read as _;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::{Duration, SystemTime};

use rsa::RsaPrivateKey;
use serde_json::Value;

use super::signing;

/// How long any one request may take to connect, or between reads.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Largest JSON document read from another server.
pub const MAX_JSON_BYTES: u64 = 1 << 20;
/// Redirects followed before giving up.
const MAX_REDIRECTS: usize = 3;

/// What ActivityPub asks for, in the two spellings servers answer to.
pub const AP_ACCEPT: &str =
    "application/activity+json, application/ld+json; profile=\"https://www.w3.org/ns/activitystreams\"";

/// Why a request to another server did not happen or did not work.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("refused to fetch {0}: {1}")]
    Refused(String, &'static str),
    #[error("{0} answered {1}")]
    Status(String, u16),
    #[error("{0}: {1}")]
    Transport(String, String),
    #[error("{0} sent something unreadable: {1}")]
    Unreadable(String, String),
}

impl FetchError {
    /// The HTTP status, when the server answered with one.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Status(_, s) => Some(*s),
            _ => None,
        }
    }
}

/// Whether an address is one anybody on the internet could reach: not this
/// machine, not a private network, not link-local, not reserved.
#[must_use]
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || o[0] == 0
                // Carrier-grade NAT, 100.64.0.0/10: Docker and Tailscale both
                // live here, and neither is the public internet.
                || (o[0] == 100 && (o[1] & 0xC0) == 64)
                // 192.0.0.0/24 (IETF protocol assignments), 198.18.0.0/15
                // (benchmarking), 240.0.0.0/4 (reserved).
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (o[1] & 0xFE) == 18)
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local fc00::/7, link-local fe80::/10, documentation
                // 2001:db8::/32, and the deprecated site-local fec0::/10.
                || (s[0] & 0xFE00) == 0xFC00
                || (s[0] & 0xFFC0) == 0xFE80
                || (s[0] & 0xFFC0) == 0xFEC0
                || (s[0] == 0x2001 && s[1] == 0x0DB8))
        }
    }
}

/// The resolver every connection goes through: a name that resolves to any
/// non-public address is refused outright rather than filtered, so a name with
/// one public and one private address cannot be used to reach the private one.
struct GuardedResolver {
    allow_private: bool,
}

impl ureq::Resolver for GuardedResolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        let addrs: Vec<SocketAddr> = netloc.to_socket_addrs()?.collect();
        if !self.allow_private && addrs.iter().any(|a| !is_public(a.ip())) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{netloc} is not a public address"),
            ));
        }
        Ok(addrs)
    }
}

/// The client, for one federation context.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    allow_private: bool,
}

impl Client {
    /// Tests only: whether this client talks to private addresses over `http`.
    #[must_use]
    pub fn allows_private(&self) -> bool {
        self.allow_private
    }

    #[must_use]
    pub fn new(allow_private: bool) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(TIMEOUT)
            .timeout_read(TIMEOUT)
            .timeout_write(TIMEOUT)
            .redirects(0)
            .resolver(GuardedResolver { allow_private })
            .user_agent(concat!(
                "cartopolis/",
                env!("CARGO_PKG_VERSION"),
                " (+https://cartopolis.org)"
            ))
            .build();
        Self {
            agent,
            allow_private,
        }
    }

    /// Whether a URL may be requested at all: `https` on the default port (or,
    /// in tests, `http` anywhere).
    pub fn check_url(&self, url: &str) -> Result<url::Url, FetchError> {
        let parsed =
            url::Url::parse(url).map_err(|_| FetchError::Refused(url.into(), "not a URL"))?;
        if parsed.host_str().is_none() || !parsed.username().is_empty() {
            return Err(FetchError::Refused(url.into(), "no host"));
        }
        if self.allow_private {
            if matches!(parsed.scheme(), "http" | "https") {
                return Ok(parsed);
            }
            return Err(FetchError::Refused(url.into(), "not http"));
        }
        if parsed.scheme() != "https" {
            return Err(FetchError::Refused(url.into(), "not https"));
        }
        if parsed.port().is_some_and(|p| p != 443) {
            return Err(FetchError::Refused(url.into(), "not the default port"));
        }
        Ok(parsed)
    }

    /// GET `url`, signed by `signer` when given (servers in "authorized fetch"
    /// mode answer nothing unsigned), following up to three redirects. Answers
    /// the bytes and the URL they finally came from.
    pub fn get(
        &self,
        url: &str,
        accept: &str,
        signer: Option<(&RsaPrivateKey, &str)>,
        max_bytes: u64,
    ) -> Result<(Vec<u8>, String), FetchError> {
        let mut current = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            let parsed = self.check_url(&current)?;
            let mut req = self.agent.get(parsed.as_str()).set("Accept", accept);
            if let Some((key, key_id)) = signer {
                for (k, v) in signing::sign(
                    key,
                    key_id,
                    "GET",
                    &host_header(&parsed),
                    &path_and_query(&parsed),
                    None,
                    SystemTime::now(),
                ) {
                    req = req.set(k, &v);
                }
            }
            match req.call() {
                Ok(resp) if (300..400).contains(&resp.status()) => {
                    let Some(location) = resp.header("location") else {
                        return Err(FetchError::Status(current, resp.status()));
                    };
                    current = parsed
                        .join(location)
                        .map_err(|_| FetchError::Refused(location.into(), "bad redirect"))?
                        .to_string();
                }
                Ok(resp) => {
                    let mut bytes = Vec::new();
                    resp.into_reader()
                        .take(max_bytes + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|e| FetchError::Transport(current.clone(), e.to_string()))?;
                    if bytes.len() as u64 > max_bytes {
                        return Err(FetchError::Refused(current, "too large"));
                    }
                    return Ok((bytes, current));
                }
                Err(ureq::Error::Status(code, _)) => {
                    return Err(FetchError::Status(current, code));
                }
                Err(e) => return Err(FetchError::Transport(current, e.to_string())),
            }
        }
        Err(FetchError::Refused(url.into(), "too many redirects"))
    }

    /// A JSON request with the caller's own headers, and a form body when
    /// given: what an OAuth round trip needs (`signin::providers`) — a code
    /// exchanged for a token, then the person's profile read with it. No
    /// redirects are followed; none of those endpoints answers with one.
    pub fn call_json(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        form: Option<&[(&str, &str)]>,
    ) -> Result<Value, FetchError> {
        let parsed = self.check_url(url)?;
        let mut req = self
            .agent
            .request(method, parsed.as_str())
            .set("Accept", "application/json");
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let answer = match form {
            Some(form) => req.send_form(form),
            None => req.call(),
        };
        match answer {
            Ok(resp) => {
                let mut bytes = Vec::new();
                resp.into_reader()
                    .take(MAX_JSON_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| FetchError::Transport(url.into(), e.to_string()))?;
                if bytes.len() as u64 > MAX_JSON_BYTES {
                    return Err(FetchError::Refused(url.into(), "too large"));
                }
                serde_json::from_slice(&bytes)
                    .map_err(|e| FetchError::Unreadable(url.into(), e.to_string()))
            }
            Err(ureq::Error::Status(code, resp)) => {
                // The body says why (a wrong secret, a redirect address the
                // provider does not know); worth the log, never the caller.
                let body = resp.into_string().unwrap_or_default();
                tracing::debug!(%url, code, body = %body.chars().take(300).collect::<String>(), "refused");
                Err(FetchError::Status(url.into(), code))
            }
            Err(e) => Err(FetchError::Transport(url.into(), e.to_string())),
        }
    }

    /// GET an ActivityPub document as JSON.
    pub fn get_json(
        &self,
        url: &str,
        signer: Option<(&RsaPrivateKey, &str)>,
    ) -> Result<Value, FetchError> {
        let (bytes, from) = self.get(url, AP_ACCEPT, signer, MAX_JSON_BYTES)?;
        serde_json::from_slice(&bytes).map_err(|e| FetchError::Unreadable(from, e.to_string()))
    }

    /// POST an activity to an inbox, signed by the actor it is from.
    pub fn post_activity(
        &self,
        inbox: &str,
        body: &[u8],
        key: &RsaPrivateKey,
        key_id: &str,
    ) -> Result<u16, FetchError> {
        let parsed = self.check_url(inbox)?;
        let mut req = self
            .agent
            .post(parsed.as_str())
            .set("Content-Type", "application/activity+json");
        for (k, v) in signing::sign(
            key,
            key_id,
            "POST",
            &host_header(&parsed),
            &path_and_query(&parsed),
            Some(body),
            SystemTime::now(),
        ) {
            req = req.set(k, &v);
        }
        match req.send_bytes(body) {
            Ok(resp) => Ok(resp.status()),
            Err(ureq::Error::Status(code, _)) => Err(FetchError::Status(inbox.into(), code)),
            Err(e) => Err(FetchError::Transport(inbox.into(), e.to_string())),
        }
    }

    /// Find an account or community by its address (`name@server`), through
    /// the server's WebFinger: the URL of its actor.
    pub fn webfinger(&self, address: &str) -> Result<String, FetchError> {
        let address = address.trim().trim_start_matches(['@', '!']);
        let (name, host) = address
            .split_once('@')
            .ok_or_else(|| FetchError::Refused(address.into(), "not name@server"))?;
        if name.is_empty() || host.is_empty() || host.contains(['/', '?', '#', '@']) {
            return Err(FetchError::Refused(address.into(), "not name@server"));
        }
        let scheme = if self.allow_private && host.contains(':') {
            "http"
        } else {
            "https"
        };
        let url = format!("{scheme}://{host}/.well-known/webfinger?resource=acct:{name}@{host}");
        let (bytes, from) = self.get(
            &url,
            "application/jrd+json, application/json",
            None,
            MAX_JSON_BYTES,
        )?;
        let doc: Value = serde_json::from_slice(&bytes)
            .map_err(|e| FetchError::Unreadable(from.clone(), e.to_string()))?;
        doc["links"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|l| {
                l["rel"] == "self"
                    && l["type"]
                        .as_str()
                        .is_some_and(|t| t.contains("activity+json") || t.contains("ld+json"))
            })
            .and_then(|l| l["href"].as_str())
            .map(str::to_string)
            .ok_or(FetchError::Unreadable(from, "no actor link".into()))
    }
}

/// The `Host` header ureq sends for a URL: the host, and the port when it is
/// not the scheme's default. What the receiving server signs against.
#[must_use]
pub fn host_header(url: &url::Url) -> String {
    match url.port() {
        Some(p) => format!("{}:{p}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_string(),
    }
}

/// The request target a signature covers.
#[must_use]
pub fn path_and_query(url: &url::Url) -> String {
    match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    }
}

/// The host of a URL, lower-cased: what a block list names.
#[must_use]
pub fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_public_internet_is_public() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.17.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.100.1.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["1.1.1.1", "185.199.108.153", "2606:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn only_https_on_its_own_port_is_fetched() {
        let c = Client::new(false);
        assert!(c.check_url("https://mastodon.example/users/a").is_ok());
        assert!(c.check_url("http://mastodon.example/users/a").is_err());
        assert!(c
            .check_url("https://mastodon.example:8080/users/a")
            .is_err());
        assert!(c.check_url("file:///etc/passwd").is_err());
        assert!(c.check_url("https://user@mastodon.example/").is_err());
        let test = Client::new(true);
        assert!(test.check_url("http://127.0.0.1:4748/ap/actor").is_ok());
    }

    #[test]
    fn a_name_on_a_private_address_is_refused_before_connecting() {
        let c = Client::new(false);
        let err = c
            .get("https://localhost/x", AP_ACCEPT, None, 10)
            .expect_err("localhost must be refused");
        assert!(matches!(err, FetchError::Transport(..)), "{err}");
        assert!(err.to_string().contains("not a public address"), "{err}");
    }

    #[test]
    fn a_signed_request_covers_the_port_it_was_sent_to() {
        let u = url::Url::parse("http://127.0.0.1:4748/ap/inbox?x=1").unwrap();
        assert_eq!(host_header(&u), "127.0.0.1:4748");
        assert_eq!(path_and_query(&u), "/ap/inbox?x=1");
        let u = url::Url::parse("https://cartopolis.org/ap/inbox").unwrap();
        assert_eq!(host_header(&u), "cartopolis.org");
    }
}
