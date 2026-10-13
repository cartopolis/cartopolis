//! Keys and HTTP signatures: how a server on the fediverse knows a request
//! came from who it says.
//!
//! Every actor this server speaks as (the server itself, each account that has
//! posted, each community with posts on) has an RSA key pair, made the first
//! time it is needed and kept in `fedi_keys`. Its public half is published on
//! the actor; every request this server sends to another is signed with the
//! private half, and every request another server sends here must be signed by
//! the actor it claims to come from.
//!
//! The scheme is the one the whole fediverse actually speaks: draft-cavage
//! HTTP Signatures with `rsa-sha256`, over `(request-target)`, `host`, `date`
//! and, for a body, `digest`. RSA because it is the only key type every server
//! verifies; the newer RFC 9421 signatures are not yet accepted widely enough
//! to send.
//!
//! **A known weakness of the `rsa` crate** (RUSTSEC-2023-0071, the "Marvin"
//! timing attack) applies to private-key operations. Signing here uses the
//! randomised (blinded) signer, the network adds milliseconds of noise to every
//! measurement, and the same crate signs for Lemmy; it is the best pure-Rust
//! option there is, and OpenSSL is what this workspace avoids everywhere else.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::pkcs8::{
    DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding,
};
use rsa::signature::{RandomizedSigner, SignatureEncoding, Verifier};
use rsa::{RsaPrivateKey, RsaPublicKey};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime};

/// Bits in a key this server makes: what Mastodon makes.
const KEY_BITS: usize = 2048;

/// How old a signed request may be. Mastodon's own window, so a server that
/// retries a delivery for a while is still believed.
const MAX_AGE: Duration = Duration::from_secs(12 * 3600);
/// How far in the future a request's date may be: clocks drift.
const MAX_SKEW: Duration = Duration::from_secs(3600);

/// The key `actor` (`instance`, `u:<account>`, `c:<community>`) signs with,
/// made and stored the first time it is asked for.
pub fn private_key(conn: &Connection, actor: &str, now_ms: i64) -> anyhow::Result<RsaPrivateKey> {
    let pem: Option<String> = conn
        .query_row(
            "SELECT private_pem FROM fedi_keys WHERE actor = ?1",
            params![actor],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(pem) = pem {
        return Ok(RsaPrivateKey::from_pkcs8_pem(&pem)?);
    }
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, KEY_BITS)?;
    let private_pem = key.to_pkcs8_pem(LineEnding::LF)?;
    let public_pem = key.to_public_key().to_public_key_pem(LineEnding::LF)?;
    // Two requests making the same actor's first key at once: the first one
    // stored wins, and the other reads it back.
    conn.execute(
        "INSERT OR IGNORE INTO fedi_keys (actor, private_pem, public_pem, created_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![actor, private_pem.as_str(), public_pem, now_ms],
    )?;
    let pem: String = conn.query_row(
        "SELECT private_pem FROM fedi_keys WHERE actor = ?1",
        params![actor],
        |r| r.get(0),
    )?;
    Ok(RsaPrivateKey::from_pkcs8_pem(&pem)?)
}

/// The public key `actor` publishes, as PEM, made if it has none yet.
pub fn public_pem(conn: &Connection, actor: &str, now_ms: i64) -> anyhow::Result<String> {
    let pem: Option<String> = conn
        .query_row(
            "SELECT public_pem FROM fedi_keys WHERE actor = ?1",
            params![actor],
            |r| r.get(0),
        )
        .optional()?;
    match pem {
        Some(p) => Ok(p),
        None => Ok(private_key(conn, actor, now_ms)?
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)?),
    }
}

/// Read a public key in either of the two PEM spellings servers publish.
pub fn parse_public_pem(pem: &str) -> Option<RsaPublicKey> {
    use rsa::pkcs1::DecodeRsaPublicKey;
    RsaPublicKey::from_public_key_pem(pem.trim())
        .ok()
        .or_else(|| RsaPublicKey::from_pkcs1_pem(pem.trim()).ok())
}

/// `SHA-256=<base64>`, the `Digest` header for a body.
#[must_use]
pub fn digest(body: &[u8]) -> String {
    format!("SHA-256={}", B64.encode(Sha256::digest(body)))
}

/// The headers to add to a request: `Date`, `Digest` (with a body) and
/// `Signature`.
pub fn sign(
    key: &RsaPrivateKey,
    key_id: &str,
    method: &str,
    host: &str,
    path_and_query: &str,
    body: Option<&[u8]>,
    now: SystemTime,
) -> Vec<(&'static str, String)> {
    let date = httpdate::fmt_http_date(now);
    let mut headers = vec![("Date", date.clone())];
    let mut names = vec!["(request-target)", "host", "date"];
    let mut lines = vec![
        format!(
            "(request-target): {} {}",
            method.to_ascii_lowercase(),
            path_and_query
        ),
        format!("host: {host}"),
        format!("date: {date}"),
    ];
    if let Some(body) = body {
        let d = digest(body);
        names.push("digest");
        lines.push(format!("digest: {d}"));
        headers.push(("Digest", d));
    }
    let signer = SigningKey::<Sha256>::new(key.clone());
    let signature = signer.sign_with_rng(&mut rsa::rand_core::OsRng, lines.join("\n").as_bytes());
    headers.push((
        "Signature",
        format!(
            "keyId=\"{key_id}\",algorithm=\"rsa-sha256\",headers=\"{}\",signature=\"{}\"",
            names.join(" "),
            B64.encode(signature.to_bytes())
        ),
    ));
    headers
}

/// A `Signature` header, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHeader {
    pub key_id: String,
    pub headers: Vec<String>,
    pub signature: Vec<u8>,
}

/// Read a `Signature` header (or the value of an `Authorization: Signature`).
pub fn parse_signature(value: &str) -> Option<SignatureHeader> {
    let value = value.trim();
    let value = value.strip_prefix("Signature ").unwrap_or(value);
    let mut key_id = None;
    let mut headers = None;
    let mut signature = None;
    let mut algorithm = None;
    for part in split_params(value) {
        let (k, v) = part.split_once('=')?;
        let v = v.trim().trim_matches('"');
        match k.trim() {
            "keyId" => key_id = Some(v.to_string()),
            "headers" => {
                headers = Some(v.split_whitespace().map(str::to_ascii_lowercase).collect())
            }
            "signature" => signature = B64.decode(v).ok(),
            "algorithm" => algorithm = Some(v.to_ascii_lowercase()),
            _ => {}
        }
    }
    // `hs2019` means "whatever the key is", which for every key this code can
    // verify is RSA with SHA-256.
    if algorithm.is_some_and(|a| a != "rsa-sha256" && a != "hs2019") {
        return None;
    }
    Some(SignatureHeader {
        key_id: key_id?,
        // The draft's default when the list is missing.
        headers: headers.unwrap_or_else(|| vec!["date".into()]),
        signature: signature?,
    })
}

/// Split `a="x,y",b="z"` on the commas outside quotes.
fn split_params(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

/// Why a signed request was not believed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Unverified {
    #[error("no signature")]
    Missing,
    #[error("the signature does not cover {0}")]
    NotCovered(&'static str),
    #[error("the request is too old or from the future")]
    Stale,
    #[error("the body is not the one that was signed")]
    BadDigest,
    #[error("the signature does not match the key")]
    BadSignature,
    #[error("the key is not one this server can read")]
    BadKey,
}

/// Check a request's signature against a public key. `header` reads a request
/// header by its lower-case name.
pub fn verify(
    sig: &SignatureHeader,
    public_pem: &str,
    method: &str,
    path_and_query: &str,
    header: impl Fn(&str) -> Option<String>,
    body: Option<&[u8]>,
    now: SystemTime,
) -> Result<(), Unverified> {
    for required in ["(request-target)", "host", "date"] {
        if !sig.headers.iter().any(|h| h == required) {
            return Err(Unverified::NotCovered(required));
        }
    }
    let date = header("date").ok_or(Unverified::NotCovered("date"))?;
    let at = httpdate::parse_http_date(&date).map_err(|_| Unverified::Stale)?;
    let too_old = now.duration_since(at).is_ok_and(|age| age > MAX_AGE);
    let too_new = at.duration_since(now).is_ok_and(|ahead| ahead > MAX_SKEW);
    if too_old || too_new {
        return Err(Unverified::Stale);
    }
    if let Some(body) = body {
        if !sig.headers.iter().any(|h| h == "digest") {
            return Err(Unverified::NotCovered("digest"));
        }
        let claimed = header("digest").ok_or(Unverified::NotCovered("digest"))?;
        let ours = digest(body);
        let matches = claimed
            .split(',')
            .any(|d| d.trim().eq_ignore_ascii_case(&ours) || d.trim() == ours);
        if !matches {
            return Err(Unverified::BadDigest);
        }
    }
    let mut lines = Vec::with_capacity(sig.headers.len());
    for name in &sig.headers {
        if name == "(request-target)" {
            lines.push(format!(
                "(request-target): {} {}",
                method.to_ascii_lowercase(),
                path_and_query
            ));
        } else {
            let v = header(name).ok_or(Unverified::BadSignature)?;
            lines.push(format!("{name}: {}", v.trim()));
        }
    }
    let key = parse_public_pem(public_pem).ok_or(Unverified::BadKey)?;
    let signature =
        Signature::try_from(sig.signature.as_slice()).map_err(|_| Unverified::BadSignature)?;
    VerifyingKey::<Sha256>::new(key)
        .verify(lines.join("\n").as_bytes(), &signature)
        .map_err(|_| Unverified::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn headers_of(signed: &[(&'static str, String)], host: &str) -> HashMap<String, String> {
        let mut h: HashMap<String, String> = signed
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        h.insert("host".into(), host.into());
        h
    }

    #[test]
    fn a_key_is_made_once_and_kept() {
        let conn = crate::db::open_and_migrate(":memory:").unwrap();
        let a = private_key(&conn, "instance", 1).unwrap();
        let b = private_key(&conn, "instance", 2).unwrap();
        assert_eq!(a, b);
        let pem = public_pem(&conn, "instance", 3).unwrap();
        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----"), "{pem}");
        assert_eq!(parse_public_pem(&pem), Some(a.to_public_key()));
    }

    #[test]
    fn a_signed_post_verifies_and_a_changed_one_does_not() {
        let conn = crate::db::open_and_migrate(":memory:").unwrap();
        let key = private_key(&conn, "c:x", 1).unwrap();
        let pem = public_pem(&conn, "c:x", 1).unwrap();
        let now = SystemTime::now();
        let body = br#"{"type":"Follow"}"#;
        let signed = sign(
            &key,
            "https://a.test/ap/c/x#main-key",
            "POST",
            "b.test",
            "/inbox",
            Some(body),
            now,
        );
        let h = headers_of(&signed, "b.test");
        let sig = parse_signature(&h["signature"]).unwrap();
        assert_eq!(sig.key_id, "https://a.test/ap/c/x#main-key");
        let get = |n: &str| h.get(n).cloned();
        assert_eq!(
            verify(&sig, &pem, "POST", "/inbox", get, Some(body), now),
            Ok(())
        );
        // Another body, another path, another day: each is refused.
        assert_eq!(
            verify(&sig, &pem, "POST", "/inbox", get, Some(b"{}"), now),
            Err(Unverified::BadDigest)
        );
        assert_eq!(
            verify(&sig, &pem, "POST", "/other", get, Some(body), now),
            Err(Unverified::BadSignature)
        );
        assert_eq!(
            verify(
                &sig,
                &pem,
                "POST",
                "/inbox",
                get,
                Some(body),
                now + Duration::from_secs(13 * 3600)
            ),
            Err(Unverified::Stale)
        );
        // Somebody else's key does not verify it.
        let other = public_pem(&conn, "c:y", 1).unwrap();
        assert_eq!(
            verify(&sig, &other, "POST", "/inbox", get, Some(body), now),
            Err(Unverified::BadSignature)
        );
    }

    #[test]
    fn a_signature_header_reads_whatever_the_order() {
        let sig = parse_signature(
            r#"signature="AAEC",headers="(request-target) host date",keyId="https://m.test/users/a#main-key",algorithm="hs2019""#,
        )
        .unwrap();
        assert_eq!(sig.key_id, "https://m.test/users/a#main-key");
        assert_eq!(sig.headers, ["(request-target)", "host", "date"]);
        assert_eq!(sig.signature, vec![0, 1, 2]);
        assert!(parse_signature(r#"keyId="k",algorithm="ed25519",signature="AA==""#).is_none());
    }
}
