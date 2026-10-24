//! Sealing the client secrets providers issue, before they go in the database.
//!
//! An operator pastes a Discord or Google client secret into Admin › Sign-in,
//! and it has to be kept: every sign-in spends it. Stored as it is, a copy of
//! `world.db` — a backup, a file handed over to debug something — would carry
//! the power to act as this server towards Discord. So it is sealed with a key
//! that lives outside the database (`signin.key` beside it, or
//! `CARTO_SIGNIN_KEY`), and the provider's id is bound in as associated data so
//! one row's secret cannot be moved to another's.
//!
//! ChaCha20-Poly1305 with a random 96-bit nonce per seal. A random nonce is safe
//! for well past 2^32 seals under one key, and this seals a handful a year.

use std::path::Path;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

const NONCE_LEN: usize = 12;

/// The server's sealing key.
#[derive(Clone)]
pub struct SealKey([u8; 32]);

impl std::fmt::Debug for SealKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealKey(..)")
    }
}

impl SealKey {
    /// From 64 hex digits.
    pub fn from_hex(text: &str) -> anyhow::Result<Self> {
        let bytes = crate::vapid::hex_decode(text)
            .filter(|b| b.len() == 32)
            .ok_or_else(|| anyhow::anyhow!("not 64 hex digits"))?;
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        Ok(Self(key))
    }

    /// A key for the life of the process: an in-memory database has nowhere to
    /// keep one, and nothing it sealed outlives it either.
    pub fn ephemeral() -> anyhow::Result<Self> {
        Ok(Self(fresh()?))
    }

    /// The key kept at `path`, made and written there (readable by this user
    /// only) the first time.
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        if let Ok(text) = std::fs::read_to_string(path) {
            return Self::from_hex(text.trim());
        }
        let key = fresh()?;
        let text: String = key.iter().map(|b| format!("{b:02x}")).collect();
        write_private(path, &text)?;
        tracing::info!(path = %path.display(), "made this server's sign-in key");
        Ok(Self(key))
    }

    /// `nonce || ciphertext` of `plain`, bound to `context` (the provider id).
    pub fn seal(&self, plain: &str, context: &str) -> anyhow::Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.0));
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plain.as_bytes(),
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| anyhow::anyhow!("could not seal"))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// The secret [`Self::seal`] sealed, or an error when the key, the context
    /// or the bytes are not the ones it was sealed with — which is what a
    /// database moved to a server with a different key looks like.
    pub fn open(&self, sealed: &[u8], context: &str) -> anyhow::Result<String> {
        if sealed.len() < NONCE_LEN {
            anyhow::bail!("sealed secret too short");
        }
        let (nonce, body) = sealed.split_at(NONCE_LEN);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.0));
        let plain = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: body,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| anyhow::anyhow!("this secret was sealed with another key"))?;
        Ok(String::from_utf8(plain)?)
    }
}

fn fresh() -> anyhow::Result<[u8; 32]> {
    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(key)
}

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

    #[test]
    fn a_sealed_secret_opens_only_with_its_key_and_its_provider() {
        let key = SealKey::ephemeral().unwrap();
        let sealed = key.seal("hunter2", "discord").unwrap();
        assert!(!sealed.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(key.open(&sealed, "discord").unwrap(), "hunter2");
        assert!(key.open(&sealed, "google").is_err());
        assert!(SealKey::ephemeral()
            .unwrap()
            .open(&sealed, "discord")
            .is_err());
    }

    #[test]
    fn the_key_file_is_made_once_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signin.key");
        let first = SealKey::load_or_create(&path).unwrap();
        let sealed = first.seal("s", "x").unwrap();
        let again = SealKey::load_or_create(&path).unwrap();
        assert_eq!(again.open(&sealed, "x").unwrap(), "s");
    }
}
