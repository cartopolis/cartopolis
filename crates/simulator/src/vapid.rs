//! This server's Web Push identity (VAPID, RFC 8292), so a browser can be
//! rung for a call while the chat page is closed (`docs/notes/calls.md`).
//!
//! A browser's push service — Google's for Chrome and Edge, Mozilla's for
//! Firefox, Apple's for Safari — delivers only pushes signed by the key the
//! page subscribed with. So the server holds one P-256 key: the page fetches
//! its public half (`GET /api/push/key`) and subscribes with it, and every push
//! to a browser carries a short-lived token signed with the private half.
//!
//! **The push is still empty**, as it is to a phone (`push.rs`): no payload, so
//! there is nothing to encrypt and nothing for the push service to read. The
//! token names this server (its key, and a contact address the services ask
//! for), which the push service learns anyway from where the push comes from.
//!
//! The key is made once and kept in a file beside the world database, because
//! every browser's subscription is bound to it: a new key on each start would
//! silently stop every subscribed browser ringing until it subscribed again.

use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};

/// How long a push token is good for. The services accept up to 24 hours; a
/// token is made per push, so this only needs to outlast the request.
const TOKEN_SECS: u64 = 12 * 3600;

pub struct Vapid {
    key: SigningKey,
    /// The public key as a browser wants it: the uncompressed point,
    /// base64url without padding.
    public: String,
    /// Who to contact about this server's pushes (the `sub` claim), a
    /// `mailto:` or `https:` address. Apple refuses a push without one.
    contact: String,
}

impl Vapid {
    /// From a 32-byte secret.
    pub fn from_secret(secret: &[u8], contact: &str) -> anyhow::Result<Self> {
        let key = SigningKey::from_slice(secret)
            .map_err(|_| anyhow::anyhow!("not a P-256 secret key"))?;
        let public = B64.encode(key.verifying_key().to_encoded_point(false).as_bytes());
        Ok(Self {
            key,
            public,
            contact: contact.to_owned(),
        })
    }

    /// A key that lives only as long as the process, for a server with no
    /// database file to keep one beside (the tests' in-memory worlds).
    pub fn ephemeral(contact: &str) -> anyhow::Result<Self> {
        Self::from_secret(&fresh_secret()?, contact)
    }

    /// The key kept at `path`, made and written there the first time.
    pub fn load_or_create(path: &Path, contact: &str) -> anyhow::Result<Self> {
        if let Ok(text) = std::fs::read_to_string(path) {
            let secret = hex_decode(text.trim())
                .ok_or_else(|| anyhow::anyhow!("{} is not a hex key", path.display()))?;
            return Self::from_secret(&secret, contact);
        }
        let secret = fresh_secret()?;
        let text: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        write_private(path, &text)?;
        tracing::info!(path = %path.display(), "made this server's web push key");
        Self::from_secret(&secret, contact)
    }

    /// The public key, as `PushManager.subscribe` takes it.
    pub fn public_key(&self) -> &str {
        &self.public
    }

    /// The `Authorization` header for a push to `endpoint`, or `None` for an
    /// address that is not an `https:` URL.
    pub fn authorization(&self, endpoint: &str, now_s: u64) -> Option<String> {
        let audience = origin(endpoint)?;
        let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({
            "aud": audience,
            "exp": now_s + TOKEN_SECS,
            "sub": self.contact,
        });
        let claims = B64.encode(claims.to_string().as_bytes());
        let signed = format!("{header}.{claims}");
        let signature: Signature = self.key.sign(signed.as_bytes());
        let token = format!("{signed}.{}", B64.encode(signature.to_bytes()));
        Some(format!("vapid t={token}, k={}", self.public))
    }
}

/// `https://host[:port]` of a URL: what a push token is addressed to.
fn origin(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| format!("https://{host}"))
}

fn fresh_secret() -> anyhow::Result<[u8; 32]> {
    let mut secret = [0u8; 32];
    // A uniformly random 32 bytes is a valid P-256 scalar except with
    // probability ~2^-32; `from_slice` would refuse the rare one, so draw again.
    loop {
        getrandom::getrandom(&mut secret).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
        if SigningKey::from_slice(&secret).is_ok() {
            return Ok(secret);
        }
    }
}

pub(crate) fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Write a secret readable by this user only.
fn write_private(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    #[test]
    fn a_push_token_is_signed_by_the_key_the_browser_was_given() {
        let vapid = Vapid::ephemeral("https://cartopolis.org").unwrap();
        let header = vapid
            .authorization("https://fcm.googleapis.com/fcm/send/abc:def", 1_000)
            .unwrap();
        let (token, key) = header
            .strip_prefix("vapid t=")
            .unwrap()
            .split_once(", k=")
            .unwrap();
        assert_eq!(key, vapid.public_key());
        let public = B64.decode(key).unwrap();
        assert_eq!(public.len(), 65, "an uncompressed P-256 point");
        let verifier = VerifyingKey::from_sec1_bytes(&public).unwrap();

        let (signed, signature) = token.rsplit_once('.').unwrap();
        let signature = Signature::from_slice(&B64.decode(signature).unwrap()).unwrap();
        verifier.verify(signed.as_bytes(), &signature).unwrap();

        let claims = signed.split('.').nth(1).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&B64.decode(claims).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["exp"], 1_000 + TOKEN_SECS);
        assert_eq!(claims["sub"], "https://cartopolis.org");
    }

    #[test]
    fn only_an_https_address_gets_a_token() {
        let vapid = Vapid::ephemeral("https://cartopolis.org").unwrap();
        assert!(vapid.authorization("http://push.example/x", 0).is_none());
        assert!(vapid.authorization("https:///x", 0).is_none());
        assert_eq!(
            origin("https://web.push.apple.com:443/QG?x=1").as_deref(),
            Some("https://web.push.apple.com:443")
        );
    }

    #[test]
    fn the_key_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vapid.key");
        let first = Vapid::load_or_create(&path, "https://cartopolis.org").unwrap();
        let again = Vapid::load_or_create(&path, "https://cartopolis.org").unwrap();
        assert_eq!(first.public_key(), again.public_key());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the secret is readable by the server only"
            );
        }
    }
}
