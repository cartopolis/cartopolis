//! The owner's release key: what approves a release of the app and of the
//! self-hosted server (`docs/mockups/release-approval`).
//!
//! On Android the key is `ReleaseKey.java`'s: ECDSA P-256 in the phone's
//! Keystore, never leaving it, signing only after a strong biometric through
//! Android's own sheet. Elsewhere there is no key and every call says so: a
//! release is approved on the owner's phone and nowhere else.

/// Where a signature is.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Signing {
    /// Nothing asked.
    #[default]
    Idle,
    /// The fingerprint sheet is up.
    Asking,
    /// Signed: the ASN.1 ECDSA signature, base64.
    Signed(String),
    /// The person said no.
    Cancelled,
    Failed(String),
}

impl Signing {
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn parse(s: Option<&str>) -> Self {
        let Some(s) = s else { return Self::Idle };
        let (word, rest) = s.split_once(' ').unwrap_or((s, ""));
        match word {
            "signing" => Self::Asking,
            "signed" => Self::Signed(rest.to_string()),
            "cancelled" => Self::Cancelled,
            "failed" => Self::Failed(rest.to_string()),
            _ => Self::Idle,
        }
    }
}

#[cfg(not(target_os = "android"))]
const ONLY_ON_A_PHONE: &str = "the release key lives on the owner's Android phone";

/// Whether this platform can hold a release key at all.
pub fn available() -> bool {
    cfg!(target_os = "android")
}

/// The public key, as the 65-byte uncompressed point in hex (what the server's
/// `RELEASE_KEYS` holds), or `None` when this phone has none yet.
pub fn public_key() -> Result<Option<String>, String> {
    #[cfg(target_os = "android")]
    {
        super::android::release_key_public()
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(None)
    }
}

/// Where the key lives: `strongbox`, `tee`, or `software`.
pub fn location() -> Option<String> {
    #[cfg(target_os = "android")]
    {
        super::android::release_key_where().ok().flatten()
    }
    #[cfg(not(target_os = "android"))]
    {
        None
    }
}

/// Make the key if there is none; its public key.
pub fn create() -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        super::android::release_key_create()
    }
    #[cfg(not(target_os = "android"))]
    {
        Err(ONLY_ON_A_PHONE.into())
    }
}

/// Ask for the owner's fingerprint and sign `data`; poll [`status`].
pub fn sign(data: &[u8], title: &str, what: &str) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        super::android::release_key_sign(data, title, what)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (data, title, what);
        Err(ONLY_ON_A_PHONE.into())
    }
}

/// How the last [`sign`] went. Reading a finished outcome clears it.
pub fn status() -> Signing {
    #[cfg(target_os = "android")]
    {
        let s = super::android::release_key_status().ok().flatten();
        let signing = Signing::parse(s.as_deref());
        if !matches!(signing, Signing::Idle | Signing::Asking) {
            let _ = super::android::release_key_clear();
        }
        signing
    }
    #[cfg(not(target_os = "android"))]
    {
        Signing::Idle
    }
}

/// The owner's Claude API key, kept encrypted beside the release key: what the
/// review of a release is asked with.
pub fn api_key() -> Option<String> {
    #[cfg(target_os = "android")]
    {
        super::android::release_api_key()
            .ok()
            .flatten()
            .filter(|k| !k.is_empty())
    }
    #[cfg(not(target_os = "android"))]
    {
        None
    }
}

/// Keep the Claude API key; an empty one forgets it.
pub fn save_api_key(key: &str) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        super::android::release_save_api_key(key.trim())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = key;
        Err(ONLY_ON_A_PHONE.into())
    }
}

/// The commit of the last release this phone approved: where the next review's
/// changes start.
pub fn last_approved() -> Option<String> {
    #[cfg(target_os = "android")]
    {
        super::android::release_last_approved().ok().flatten()
    }
    #[cfg(not(target_os = "android"))]
    {
        None
    }
}

pub fn save_last_approved(commit: &str) {
    #[cfg(target_os = "android")]
    {
        if let Err(e) = super::android::release_save_last_approved(commit) {
            tracing::warn!("could not keep the approved commit: {e}");
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = commit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_java_side_reads_back() {
        assert_eq!(Signing::parse(None), Signing::Idle);
        assert_eq!(Signing::parse(Some("signing")), Signing::Asking);
        assert_eq!(
            Signing::parse(Some("signed MEUCIQ==")),
            Signing::Signed("MEUCIQ==".into())
        );
        assert_eq!(Signing::parse(Some("cancelled")), Signing::Cancelled);
        assert_eq!(
            Signing::parse(Some("failed no key")),
            Signing::Failed("no key".into())
        );
    }
}
