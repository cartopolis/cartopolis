//! A person's chats, sealed under their recovery key, so that a new device
//! can read what was said before it existed (`docs/notes/encrypted-chat.md`,
//! "Recovering chats with a recovery key").
//!
//! The recovery key is a random secret shown once, as 24 letters and digits.
//! The backup is every chat's newest [`MAX_HISTORY`] messages, sealed under a
//! key derived from it and kept in the server's picture store, which cannot
//! open it. A device that has the recovery key re-seals and replaces it as the
//! chats change; a new device given the key opens it and puts each chat's
//! messages in front of what it reads itself.
//!
//! It carries no MLS keys. A new device is added to its chats the usual way
//! (a member's device commits it in); the backup only brings back what those
//! chats said before it.

use serde::{Deserialize, Serialize};

use crate::{HistoryItem, MAX_HISTORY};

/// Bytes of secret in a recovery key: 120 bits, 24 characters of base 32.
const SECRET_BYTES: usize = 15;

/// Crockford's base 32: no I, L, O or U, so nothing reads as something else.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The first byte of an opened backup.
pub const BACKUP_FORMAT: u8 = 1;

/// One chat in a backup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackupChat {
    /// Its channel key (`dm:<hex>`), the same on every device.
    pub channel: String,
    /// What the chat list called it when the backup was made: who a direct
    /// chat is with, or a group's name.
    pub label: String,
    /// Whether it is a group (a topic is one) rather than a direct chat.
    pub group: bool,
    /// Its newest messages, oldest first.
    pub items: Vec<HistoryItem>,
}

/// A new recovery key, as it is shown: six groups of four, dash-separated.
pub fn new_recovery_key() -> Result<String, String> {
    let secret: [u8; SECRET_BYTES] = cartopolis_e2ee::random_bytes().map_err(|e| e.to_string())?;
    Ok(spell(&secret))
}

fn spell(secret: &[u8; SECRET_BYTES]) -> String {
    let mut chars = Vec::with_capacity(SECRET_BYTES * 8 / 5);
    let (mut acc, mut bits) = (0u32, 0);
    for &b in secret {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            chars.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    chars
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// The secret a recovery key spells. Forgiving about how it was copied:
/// case, spaces and dashes do not matter, and O, I and L read as 0, 1 and 1.
pub fn parse_recovery_key(text: &str) -> Result<[u8; SECRET_BYTES], String> {
    let mut out = [0u8; SECRET_BYTES];
    let (mut acc, mut bits, mut n, mut chars) = (0u32, 0, 0usize, 0usize);
    for c in text.chars() {
        if c.is_whitespace() || c == '-' {
            continue;
        }
        let c = match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        let Some(v) = ALPHABET.iter().position(|&a| a as char == c) else {
            return Err(format!("“{c}” is not in a recovery key"));
        };
        chars += 1;
        if chars > SECRET_BYTES * 8 / 5 {
            return Err("that is longer than a recovery key".into());
        }
        acc = (acc << 5) | v as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out[n] = (acc >> bits) as u8;
            n += 1;
        }
    }
    if n < SECRET_BYTES {
        return Err("that is shorter than a recovery key: it has 24 letters and digits".into());
    }
    Ok(out)
}

/// Seal `chats` under `recovery_key`, each cut to its newest [`MAX_HISTORY`].
pub fn seal_backup(recovery_key: &str, chats: &[BackupChat]) -> Result<Vec<u8>, String> {
    let key = key_of(recovery_key)?;
    let kept: Vec<BackupChat> = chats
        .iter()
        .filter(|c| !c.items.is_empty())
        .map(|c| BackupChat {
            channel: c.channel.clone(),
            label: c.label.clone(),
            group: c.group,
            items: c.items[c.items.len().saturating_sub(MAX_HISTORY)..].to_vec(),
        })
        .collect();
    let mut plain = vec![BACKUP_FORMAT];
    plain.extend(postcard::to_allocvec(&kept).map_err(|e| e.to_string())?);
    cartopolis_e2ee::seal_with_key(&key, &plain).map_err(|e| e.to_string())
}

/// Open a backup. A wrong recovery key fails here, never as garbage.
pub fn open_backup(recovery_key: &str, sealed: &[u8]) -> Result<Vec<BackupChat>, String> {
    let key = key_of(recovery_key)?;
    let plain = cartopolis_e2ee::open_with_key(&key, sealed)
        .map_err(|_| "that recovery key does not open this backup".to_string())?;
    match plain.split_first() {
        Some((&BACKUP_FORMAT, rest)) => postcard::from_bytes(rest).map_err(|e| e.to_string()),
        Some((v, _)) => Err(format!(
            "a backup in format {v}, which this version cannot read"
        )),
        None => Err("an empty backup".into()),
    }
}

fn key_of(recovery_key: &str) -> Result<[u8; 32], String> {
    let secret = parse_recovery_key(recovery_key)?;
    cartopolis_e2ee::backup_key(&secret).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(seq: u64) -> HistoryItem {
        HistoryItem {
            seq,
            from: "ann@x".into(),
            body: format!("line {seq}"),
            sent_at_ms: seq * 1000,
            parent: None,
            point: None,
            picture: None,
        }
    }

    #[test]
    fn a_recovery_key_reads_back_however_it_was_copied() {
        let key = new_recovery_key().unwrap();
        assert_eq!(key.len(), 24 + 5, "six groups of four: {key}");
        let secret = parse_recovery_key(&key).unwrap();
        let sloppy = key.to_lowercase().replace('-', " ").replace('0', "o");
        assert_eq!(parse_recovery_key(&sloppy).unwrap(), secret);
        assert_eq!(spell(&secret), key);
        assert!(parse_recovery_key(&key[..20]).is_err());
        assert!(parse_recovery_key(&format!("{key}A")).is_err());
        assert!(parse_recovery_key("UUUU").is_err());
        assert_ne!(new_recovery_key().unwrap(), key);
    }

    #[test]
    fn a_backup_opens_with_its_recovery_key_only() {
        let key = new_recovery_key().unwrap();
        let chats = vec![
            BackupChat {
                channel: "dm:01".into(),
                label: "ann".into(),
                group: false,
                items: (0..MAX_HISTORY as u64 + 5).map(item).collect(),
            },
            BackupChat {
                channel: "dm:02".into(),
                label: "nothing said".into(),
                group: true,
                items: Vec::new(),
            },
        ];
        let sealed = seal_backup(&key, &chats).unwrap();
        let opened = open_backup(&key.to_lowercase(), &sealed).unwrap();
        assert_eq!(opened.len(), 1, "an empty chat is left out");
        assert_eq!(opened[0].items.len(), MAX_HISTORY);
        assert_eq!(opened[0].items[0].seq, 5, "the newest are kept");
        let other = new_recovery_key().unwrap();
        assert!(open_backup(&other, &sealed).is_err());
        assert_ne!(
            seal_backup(&key, &chats).unwrap(),
            sealed,
            "each seal has its own nonce"
        );
    }
}
