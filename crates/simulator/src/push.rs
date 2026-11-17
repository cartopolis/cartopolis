//! Waking a phone or browser whose app is closed when somebody calls
//! (protocol 29, `docs/notes/calls.md`) or writes in one of its private chats
//! (`docs/notes/message-wake-ups.md`).
//!
//! A connected device hears a call ring through `CallRoster`. A closed app
//! hears nothing, so each device may register a **UnifiedPush** endpoint
//! (`SetPushEndpoint`): an `https:` URL its distributor app (ntfy, say) gave
//! it. When a call *starts* in a chat, this posts to the endpoints of every
//! other member, and the app's receiver shows "Incoming call"; opening it
//! connects, and the roster rings as it always has.
//!
//! A **browser** registers the same way, its push service's address marked
//! with [`WEB_PREFIX`] (the web chat's `src/lib/web-push.ts`). Its push service
//! — Google's, Mozilla's, Apple's — takes only a push signed with the key the
//! page subscribed with (`crate::vapid`), so that one is signed.
//!
//! **A message wakes too, but only a device that says it can tell one from a
//! call** — an app or page older than this shows every push as "Incoming
//! call". A phone says so with [`NOTICES_PREFIX`]; a browser by giving its
//! subscription's keys after the address (`webpush:<url>#<p256dh>.<auth>`),
//! which it needs anyway, because a browser's push can carry a word only
//! encrypted to it ([`seal`], RFC 8291).
//!
//! **The push says which of the two it is, and nothing more.** Not who is
//! calling or writing, not which chat — the distributor's server relays it and
//! has no business knowing. A wake-up is all the phone needs, because the app
//! learns the rest over its own connection. To a browser that word is
//! encrypted; to a phone's distributor (the person's own choice, and one they
//! can run themselves) it is plain, so the distributor learns "a call" from
//! "a message", as it could from their rhythm anyway.
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

use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use tokio::sync::mpsc;

use crate::db::{self, SharedConn};
use crate::vapid::Vapid;

/// Longest endpoint accepted. A UnifiedPush endpoint is a URL with a random
/// token in it; anything longer than this is not one.
pub const MAX_ENDPOINT_LEN: usize = 1024;

/// One push per endpoint per this long: a call that is started, dropped and
/// started again rings once, not three times.
const QUIET: Duration = Duration::from_secs(20);

/// One message wake-up per account per this long. A busy chat wakes a closed
/// phone once, not at every line: the notice it shows stands until it is
/// opened, and a second push would only post the same notice again.
const MESSAGE_QUIET: Duration = Duration::from_secs(120);

/// How long a distributor gets to take the push.
const TIMEOUT: Duration = Duration::from_secs(8);

/// What a push wakes a device for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A call has started in one of its chats.
    Call,
    /// Somebody wrote in one of its private chats or topics.
    Message,
    /// The municipality published a notice near a place the account follows
    /// (`plans_watch`).
    Plan,
}

impl Kind {
    /// The word the push carries, which the receivers (`PushReceiver.java`,
    /// the web chat's `sw.js`) match on. A phone's call push stays the `1` it
    /// always was.
    fn word(self, web: bool) -> &'static [u8] {
        match (self, web) {
            (Kind::Call, false) => b"1",
            (Kind::Call, true) => b"call",
            (Kind::Message, _) => b"message",
            (Kind::Plan, _) => b"plan",
        }
    }
}

/// Something happened in a chat: wake the other members' closed apps.
#[derive(Debug, Clone)]
pub struct Ring {
    pub kind: Kind,
    /// `accounts.id` of the person who started the call or wrote — their own
    /// devices are not woken. Empty for somebody elsewhere.
    pub caller: String,
    /// Their username, for the block check.
    pub caller_name: String,
    /// The member accounts to wake (`accounts.id`): for a call every member of
    /// the chat, for a message those with no device connected.
    pub members: Vec<String>,
}

/// What a browser's subscription is registered as: its push service's URL
/// with this in front. A browser's push service takes only a signed push
/// (`vapid`) and a phone's distributor takes a plain one, and the address alone
/// cannot say which it is — Chrome's and an Android distributor's can both be
/// Google's. Carried in the string the wire already has, so the message and the
/// table are unchanged; a server older than this refuses it as "not https",
/// which is the right answer from a server that could not sign the push.
pub const WEB_PREFIX: &str = "webpush:";

/// What a phone's UnifiedPush address is registered as when its app tells a
/// message wake-up from a call (`systems::push`). An app older than that
/// registers the bare address and is woken for calls only, because it shows
/// any push as "Incoming call".
pub const NOTICES_PREFIX: &str = "notices:";

/// What a phone's address is registered as when its app also shows a plan
/// wake-up as one (`systems::push`, protocol 50). It implies [`NOTICES_PREFIX`]:
/// an app that knows "plan" knows "message". A browser is never woken for a
/// plan — the web page follows no places, and `sw.js` shows any word but
/// "message" as a call.
pub const PLANS_PREFIX: &str = "plans:";

/// What a registered endpoint says about the device behind it.
#[derive(Debug, PartialEq, Eq)]
struct Target<'a> {
    /// Where the push is posted.
    url: &'a str,
    /// A browser: every push signed (`vapid`).
    web: bool,
    /// A browser's subscription keys, base64url (RFC 8291's `p256dh` and
    /// `auth`), which it gives once it tells a message from a call.
    keys: Option<(&'a str, &'a str)>,
    /// Whether the device may be woken for a message.
    notices: bool,
    /// Whether it may be woken for a plan near a followed place.
    plans: bool,
}

fn target(endpoint: &str) -> Target<'_> {
    if let Some(web) = endpoint.strip_prefix(WEB_PREFIX) {
        let (url, keys) = match web.split_once('#') {
            Some((url, keys)) => (url, keys.split_once('.')),
            None => (web, None),
        };
        return Target {
            url,
            web: true,
            notices: keys.is_some(),
            keys,
            plans: false,
        };
    }
    if let Some(url) = endpoint.strip_prefix(PLANS_PREFIX) {
        return Target {
            url,
            web: false,
            keys: None,
            notices: true,
            plans: true,
        };
    }
    match endpoint.strip_prefix(NOTICES_PREFIX) {
        Some(url) => Target {
            url,
            web: false,
            keys: None,
            notices: true,
            plans: false,
        },
        None => Target {
            url: endpoint,
            web: false,
            keys: None,
            notices: false,
            plans: false,
        },
    }
}

/// The address a registration posts to, whichever way it is written: one
/// device registers one address, and a newer app or page writes it differently
/// from an older one.
pub fn address(endpoint: &str) -> &str {
    target(endpoint).url
}

/// Why an endpoint is refused, in words for the log.
pub fn check_endpoint(endpoint: &str) -> Result<(), &'static str> {
    if endpoint.len() > MAX_ENDPOINT_LEN {
        return Err("too long");
    }
    let t = target(endpoint);
    if t.web && endpoint.contains('#') {
        let Some((p256dh, auth)) = t.keys else {
            return Err("keys not as p256dh.auth");
        };
        let p256dh = B64.decode(p256dh).map_err(|_| "keys not base64url")?;
        let auth = B64.decode(auth).map_err(|_| "keys not base64url")?;
        if p256dh.len() != 65 || auth.len() != 16 {
            return Err("keys of the wrong length");
        }
    }
    let url = t.url;
    let Some(rest) = url.strip_prefix("https://") else {
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

/// Encrypt `plaintext` to a browser's subscription (RFC 8291, `aes128gcm` of
/// RFC 8188, one record): `ua_public` and `auth` are the subscription's
/// `p256dh` and `auth` keys, `as_secret` a key made for this one push, `salt`
/// sixteen fresh random bytes. `None` when the subscription's key is not a
/// P-256 point.
fn seal(
    ua_public: &[u8],
    auth: &[u8],
    plaintext: &[u8],
    as_secret: &p256::SecretKey,
    salt: &[u8; 16],
) -> Option<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit};
    use hkdf::Hkdf;
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    use sha2::Sha256;

    let ua = p256::PublicKey::from_sec1_bytes(ua_public).ok()?;
    let as_public = as_secret.public_key().to_encoded_point(false);
    let shared = p256::ecdh::diffie_hellman(as_secret.to_nonzero_scalar(), ua.as_affine());
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public.as_bytes());
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), shared.raw_secret_bytes())
        .expand(&key_info, &mut ikm)
        .ok()?;
    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .ok()?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).ok()?;
    // The last (and only) record ends in its delimiter, 2, and no padding.
    let mut record = plaintext.to_vec();
    record.push(2);
    let sealed = aes_gcm::Aes128Gcm::new_from_slice(&cek)
        .ok()?
        .encrypt(aes_gcm::Nonce::from_slice(&nonce), record.as_slice())
        .ok()?;
    let mut body = salt.to_vec();
    body.extend_from_slice(&4096u32.to_be_bytes());
    body.push(as_public.as_bytes().len() as u8);
    body.extend_from_slice(as_public.as_bytes());
    body.extend_from_slice(&sealed);
    Some(body)
}

/// `word` encrypted to the browser whose keys `keys` are.
fn seal_for(keys: (&str, &str), word: &[u8]) -> Option<Vec<u8>> {
    let ua_public = B64.decode(keys.0).ok()?;
    let auth = B64.decode(keys.1).ok()?;
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt).ok()?;
    let mut secret = [0u8; 32];
    getrandom::getrandom(&mut secret).ok()?;
    let as_secret = p256::SecretKey::from_slice(&secret).ok()?;
    seal(&ua_public, &auth, word, &as_secret, &salt)
}

fn post(agent: &ureq::Agent, endpoint: &str, kind: Kind, vapid: Option<&Vapid>) -> Sent {
    // RFC 8030's headers, which UnifiedPush distributors and browsers' push
    // services both honour: a call that cannot be delivered within a minute is
    // not worth delivering at all. A message can wait a day for a phone that
    // is off, and need not wake one that is dozing.
    let (ttl, urgency) = match kind {
        Kind::Call => ("60", "high"),
        Kind::Message | Kind::Plan => ("86400", "normal"),
    };
    let t = target(endpoint);
    let result = if t.web {
        let now_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let Some(auth) = vapid.and_then(|v| v.authorization(t.url, now_s)) else {
            return Sent::Failed;
        };
        let request = agent
            .post(t.url)
            .set("TTL", ttl)
            .set("Urgency", urgency)
            .set("Authorization", &auth);
        match t.keys {
            // A page that gave its keys is told which it is, encrypted to it.
            Some(keys) => {
                let Some(body) = seal_for(keys, kind.word(true)) else {
                    return Sent::Failed;
                };
                request
                    .set("Content-Encoding", "aes128gcm")
                    .set("Content-Type", "application/octet-stream")
                    .send_bytes(&body)
            }
            // An older page: no body at all, which it shows as a call.
            None => request.send_bytes(&[]),
        }
    } else {
        agent
            .post(t.url)
            .set("TTL", ttl)
            .set("Urgency", urgency)
            .set("Content-Type", "application/octet-stream")
            .send_bytes(kind.word(false))
    };
    match result {
        Ok(_) => Sent::Taken,
        Err(ureq::Error::Status(404 | 410, _)) => Sent::Gone,
        Err(e) => {
            tracing::debug!("push not taken: {e}");
            Sent::Failed
        }
    }
}

/// The endpoints a ring wakes: every member's but the caller's own, and
/// nobody's who blocked the caller — the roster does not ring them either, nor
/// does a blocked sender's message reach them. A message wakes only the
/// devices that can tell it from a call.
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
        out.extend(
            db::push_endpoints(c, account)
                .unwrap_or_default()
                .into_iter()
                .filter(|e| match ring.kind {
                    Kind::Call => true,
                    Kind::Message => target(e).notices,
                    Kind::Plan => target(e).plans,
                }),
        );
    }
    out
}

/// Start the task that sends the pushes; rings go in through the channel.
pub fn spawn(conn: SharedConn, vapid: Option<Arc<Vapid>>) -> mpsc::UnboundedSender<Ring> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(conn, vapid, rx));
    tx
}

async fn run(conn: SharedConn, vapid: Option<Arc<Vapid>>, mut rx: mpsc::UnboundedReceiver<Ring>) {
    let mut last: HashMap<String, Instant> = HashMap::new();
    let mut written_to: HashMap<String, Instant> = HashMap::new();
    while let Some(mut ring) = rx.recv().await {
        let kind = ring.kind;
        // A message wakes each account once a while, before any database work:
        // a busy topic sends one of these per line.
        if kind == Kind::Message {
            let now = Instant::now();
            written_to.retain(|_, at| now.duration_since(*at) < MESSAGE_QUIET);
            ring.members.retain(|a| !written_to.contains_key(a));
            for a in &ring.members {
                written_to.insert(a.clone(), now);
            }
            if ring.members.is_empty() {
                continue;
            }
        }
        let found = db::call(&conn, move |c| endpoints_for(c, &ring)).await;
        let Ok(endpoints) = found else { continue };
        let now = Instant::now();
        last.retain(|_, at| now.duration_since(*at) < QUIET);
        // The per-endpoint quiet is a call's: a message must not hold back
        // the ring of a call started a moment after it.
        let due: Vec<String> = endpoints
            .into_iter()
            .filter(|e| kind != Kind::Call || !last.contains_key(e))
            .filter(|e| check_endpoint(e).is_ok())
            .collect();
        if kind == Kind::Call {
            for e in &due {
                last.insert(e.clone(), now);
            }
        }
        if due.is_empty() {
            continue;
        }
        let vapid = vapid.clone();
        let results = tokio::task::spawn_blocking(move || {
            let agent = agent();
            due.into_iter()
                .map(|e| {
                    let sent = post(&agent, &e, kind, vapid.as_deref());
                    (e, sent)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let taken = results.iter().filter(|(_, s)| *s == Sent::Taken).count();
        tracing::info!(taken, of = results.len(), ?kind, "woke closed apps");
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
        // A browser's subscription, and the same checks under its prefix.
        assert!(check_endpoint("webpush:https://fcm.googleapis.com/fcm/send/a:b").is_ok());
        assert!(check_endpoint("webpush:http://fcm.googleapis.com/x").is_err());
        assert!(check_endpoint("webpush:https://user:pw@push.example/x").is_err());
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
            kind: Kind::Call,
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
    fn a_message_wakes_only_devices_that_tell_it_from_a_call() {
        let dir = tempfile::tempdir().unwrap();
        let c = db::open_and_migrate(dir.path().join("w.db").to_str().unwrap()).unwrap();
        let keys = format!("#{}.{}", "A".repeat(87), "A".repeat(22));
        let new_phone = format!("{NOTICES_PREFIX}https://push.example/bob-phone");
        let new_page = format!("{WEB_PREFIX}https://fcm.example/bob-page{keys}");
        for endpoint in [
            "https://push.example/bob-old-phone",
            "webpush:https://fcm.example/bob-old-page",
            new_phone.as_str(),
            new_page.as_str(),
        ] {
            assert!(check_endpoint(endpoint).is_ok(), "{endpoint}");
            db::set_push_endpoint(&c, "bob", endpoint, 1).unwrap();
        }
        let mut ring = Ring {
            kind: Kind::Message,
            caller: "ann".into(),
            caller_name: "ann".into(),
            members: vec!["ann".into(), "bob".into()],
        };
        let mut woken = endpoints_for(&c, &ring);
        woken.sort();
        assert_eq!(woken, vec![new_phone.clone(), new_page.clone()]);
        // A call still rings every one of them.
        ring.kind = Kind::Call;
        assert_eq!(endpoints_for(&c, &ring).len(), 4);
        // Each is posted to its bare address.
        assert_eq!(address(&new_phone), "https://push.example/bob-phone");
        assert_eq!(address(&new_page), "https://fcm.example/bob-page");
        // Keys that are not a P-256 point and a 16-byte secret are refused.
        assert!(check_endpoint("webpush:https://fcm.example/x#abc.def").is_err());
        assert!(check_endpoint("webpush:https://fcm.example/x#nodot").is_err());

        // A device that registers its address anew, now telling messages from
        // calls, is not left registered twice.
        db::set_push_endpoint(&c, "dee", "https://push.example/dee", 1).unwrap();
        db::set_push_endpoint(&c, "dee", "webpush:https://fcm.example/dee", 1).unwrap();
        let phone = format!("{NOTICES_PREFIX}https://push.example/dee");
        let page = format!("{WEB_PREFIX}https://fcm.example/dee{keys}");
        db::set_push_endpoint(&c, "dee", &phone, 2).unwrap();
        db::set_push_endpoint(&c, "dee", &page, 2).unwrap();
        assert_eq!(db::push_endpoints(&c, "dee").unwrap(), vec![phone, page]);
    }

    #[test]
    fn a_plan_wakes_only_phones_that_know_the_word() {
        let dir = tempfile::tempdir().unwrap();
        let c = db::open_and_migrate(dir.path().join("w.db").to_str().unwrap()).unwrap();
        let keys = format!("#{}.{}", "A".repeat(87), "A".repeat(22));
        let plans_phone = format!("{PLANS_PREFIX}https://push.example/eve-phone");
        for endpoint in [
            "https://push.example/eve-old-phone".to_string(),
            format!("{NOTICES_PREFIX}https://push.example/eve-tablet"),
            format!("{WEB_PREFIX}https://fcm.example/eve-page{keys}"),
            plans_phone.clone(),
        ] {
            assert!(check_endpoint(&endpoint).is_ok(), "{endpoint}");
            db::set_push_endpoint(&c, "eve", &endpoint, 1).unwrap();
        }
        let mut ring = Ring {
            kind: Kind::Plan,
            caller: String::new(),
            caller_name: String::new(),
            members: vec!["eve".into()],
        };
        assert_eq!(endpoints_for(&c, &ring), vec![plans_phone.clone()]);
        // It knows a message and a call as well.
        ring.kind = Kind::Message;
        assert!(endpoints_for(&c, &ring).contains(&plans_phone));
        assert_eq!(address(&plans_phone), "https://push.example/eve-phone");

        // The phone that knew messages and now knows plans is registered once.
        let tablet = format!("{PLANS_PREFIX}https://push.example/eve-tablet");
        db::set_push_endpoint(&c, "eve", &tablet, 2).unwrap();
        let all = db::push_endpoints(&c, "eve").unwrap();
        assert_eq!(all.len(), 4);
        assert!(!all.iter().any(|e| e.starts_with(NOTICES_PREFIX)));
    }

    /// RFC 8291's own example (appendix A): the same keys, salt and text give
    /// the same bytes, so a browser can open what this seals.
    #[test]
    fn a_word_to_a_browser_is_sealed_as_rfc_8291_says() {
        let d = |s: &str| B64.decode(s).unwrap();
        let as_secret =
            p256::SecretKey::from_slice(&d("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        let ua_public = d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let auth = d("BTBZMqHH6r4Tts7J_aSIgg");
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = seal(
            &ua_public,
            &auth,
            b"When I grow up, I want to be a watermelon",
            &as_secret,
            &salt,
        )
        .unwrap();
        assert_eq!(
            B64.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    /// A real browser push service takes the signed, empty push. By hand, as
    /// it needs the internet and a live subscription: subscribe somewhere (a
    /// browser, or Mozilla's open WebSocket protocol), then
    /// `PUSH_TEST_ENDPOINT=<its URL> PUSH_TEST_KEY=<the VAPID secret, hex>
    /// cargo test -p cartopolis_simulator --lib -- --ignored a_real_push`.
    #[test]
    #[ignore = "needs the internet and a live push subscription"]
    fn a_real_push_service_takes_a_signed_push() {
        let endpoint = std::env::var("PUSH_TEST_ENDPOINT").expect("PUSH_TEST_ENDPOINT");
        let secret =
            crate::vapid::hex_decode(&std::env::var("PUSH_TEST_KEY").expect("PUSH_TEST_KEY"))
                .expect("hex");
        let vapid = Vapid::from_secret(&secret, "https://cartopolis.org").unwrap();
        let sent = post(
            &agent(),
            &format!("{WEB_PREFIX}{endpoint}"),
            Kind::Call,
            Some(&vapid),
        );
        assert_eq!(sent, Sent::Taken);
    }

    #[test]
    fn the_resolver_refuses_a_name_for_a_private_address() {
        let err = ureq::Resolver::resolve(&PublicOnly, "127.0.0.1:443").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let err = ureq::Resolver::resolve(&PublicOnly, "localhost:443").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }
}
