//! Sealing what people in a call say to each other outside their voices — a
//! reaction (protocol 41, `NetMessage::CallReact`) — so the simulator relays it
//! without reading it, the way it forwards frames.
//!
//! The key comes from the same base key as the call's frames
//! (`cartopolis_e2ee::Device::call_epoch`), under a label of its own so an
//! event key can never be a frame key: `HMAC-SHA256(base_key, LABEL ‖ epoch LE)`,
//! first 16 bytes, for AES-128-GCM. A sealed event is a random 12-byte nonce
//! followed by the ciphertext and its tag; the epoch is the associated data,
//! so an event cannot be replayed into another epoch.
//!
//! The browser has the same in `crates/cartopolis/web/call-events.js`, held to
//! this by `crates/calls/testdata/event-vectors.json`.
//!
//! What this does not stop, as for frames (`keys`): a member sealing an event
//! as if another sent it. Who sent one is stamped by the server
//! (`CallReacted.participant`), which knows whose connection it came on.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const LABEL: &[u8] = b"cartopolis call events";
const NONCE_LEN: usize = 12;

fn key(base_key: &[u8], epoch: u64) -> Aes128Gcm {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(base_key).expect("HMAC takes any key");
    mac.update(LABEL);
    mac.update(&epoch.to_le_bytes());
    let out = mac.finalize().into_bytes();
    Aes128Gcm::new_from_slice(&out[..16]).expect("16 bytes")
}

fn seal_with(base_key: &[u8], epoch: u64, nonce: [u8; NONCE_LEN], plain: &[u8]) -> Vec<u8> {
    let aad = epoch.to_le_bytes();
    let sealed = key(base_key, epoch)
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plain,
                aad: &aad,
            },
        )
        .expect("AES-GCM seals any length a reaction has");
    let mut out = nonce.to_vec();
    out.extend(sealed);
    out
}

/// Seal `plain` for everyone holding `epoch`'s base key.
#[must_use]
pub fn seal(base_key: &[u8], epoch: u64, plain: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; NONCE_LEN];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut nonce);
    seal_with(base_key, epoch, nonce, plain)
}

/// Open what [`seal`] sealed; `None` for anything else (another epoch's key,
/// tampering, noise).
#[must_use]
pub fn open(base_key: &[u8], epoch: u64, sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE_LEN {
        return None;
    }
    let (nonce, body) = sealed.split_at(NONCE_LEN);
    let aad = epoch.to_le_bytes();
    key(base_key, epoch)
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: body,
                aad: &aad,
            },
        )
        .ok()
}

// ── A guest's key (`docs/notes/calls.md`, "Guests") ────────────────────────
//
// A guest is in no chat, so it has no base key of its own. Once a host lets it
// in, the host's device hands it the call's key, sealed under the secret in
// the guest link's `#…`, which the server never sees: HMAC-SHA256(secret,
// GUEST_LABEL), first 16 bytes, for AES-128-GCM, with the chat's id as the
// associated data so a key sealed for one chat opens in no other. The server
// relays the bytes (`CallKeyDrop` → `CallKeyDropped`) and cannot open them.

const GUEST_LABEL: &[u8] = b"cartopolis guest key";

/// The first sender index a guest is given: above any chat's member indices
/// (an MLS leaf index), so a guest's frames name a sender nobody else is.
pub const GUEST_INDEX_BASE: u32 = 0x8000;

/// The sender index of the guest seated as `participant`.
#[must_use]
pub fn guest_index(participant: u64) -> u32 {
    GUEST_INDEX_BASE | (participant as u32 & 0x7fff)
}

/// What a guest needs to take part: the epoch's base key, who sends under
/// which index, and its own index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestKey {
    pub epoch: u64,
    pub base_key: Vec<u8>,
    pub own_index: u32,
    pub members: Vec<u32>,
}

impl GuestKey {
    /// `epoch` LE (8) ‖ own index LE (4) ‖ member count LE (2) ‖ members LE
    /// (4 each) ‖ base key: bytes the browser's `DataView` reads as easily.
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(14 + 4 * self.members.len() + self.base_key.len());
        out.extend(self.epoch.to_le_bytes());
        out.extend(self.own_index.to_le_bytes());
        out.extend((self.members.len().min(u16::MAX as usize) as u16).to_le_bytes());
        for m in self.members.iter().take(u16::MAX as usize) {
            out.extend(m.to_le_bytes());
        }
        out.extend(&self.base_key);
        out
    }

    fn from_bytes(b: &[u8]) -> Option<Self> {
        let epoch = u64::from_le_bytes(b.get(..8)?.try_into().ok()?);
        let own_index = u32::from_le_bytes(b.get(8..12)?.try_into().ok()?);
        let n = usize::from(u16::from_le_bytes(b.get(12..14)?.try_into().ok()?));
        let members = (0..n)
            .map(|i| {
                let at = 14 + 4 * i;
                Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
            })
            .collect::<Option<Vec<u32>>>()?;
        let base_key = b.get(14 + 4 * n..)?.to_vec();
        (!base_key.is_empty()).then_some(Self {
            epoch,
            base_key,
            own_index,
            members,
        })
    }
}

fn guest_cipher(secret: &[u8]) -> Aes128Gcm {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC takes any key");
    mac.update(GUEST_LABEL);
    let out = mac.finalize().into_bytes();
    Aes128Gcm::new_from_slice(&out[..16]).expect("16 bytes")
}

fn seal_guest_with(secret: &[u8], group: &[u8], nonce: [u8; NONCE_LEN], key: &GuestKey) -> Vec<u8> {
    let sealed = guest_cipher(secret)
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &key.to_bytes(),
                aad: group,
            },
        )
        .expect("AES-GCM seals a key");
    let mut out = nonce.to_vec();
    out.extend(sealed);
    out
}

/// Seal a call's key for a guest of the chat `group`, under its link's secret.
#[must_use]
pub fn seal_guest(secret: &[u8], group: &[u8], key: &GuestKey) -> Vec<u8> {
    let mut nonce = [0u8; NONCE_LEN];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut nonce);
    seal_guest_with(secret, group, nonce, key)
}

/// Open what [`seal_guest`] sealed; `None` for another link, another chat,
/// or tampering.
#[must_use]
pub fn open_guest(secret: &[u8], group: &[u8], sealed: &[u8]) -> Option<GuestKey> {
    if sealed.len() < NONCE_LEN {
        return None;
    }
    let (nonce, body) = sealed.split_at(NONCE_LEN);
    let plain = guest_cipher(secret)
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: body,
                aad: group,
            },
        )
        .ok()?;
    GuestKey::from_bytes(&plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guest_key_opens_only_with_its_link_and_its_chat() {
        let key = GuestKey {
            epoch: 9,
            base_key: vec![3; 16],
            own_index: guest_index(42),
            members: vec![0, 1, 2],
        };
        let sealed = seal_guest(b"0123456789abcdef", b"chat", &key);
        assert_eq!(open_guest(b"0123456789abcdef", b"chat", &sealed), Some(key));
        assert_eq!(open_guest(b"0123456789abcdeX", b"chat", &sealed), None);
        assert_eq!(open_guest(b"0123456789abcdef", b"other", &sealed), None);
        assert_eq!(guest_index(42), 0x8000 + 42);
        assert!(guest_index(u64::MAX) >= GUEST_INDEX_BASE);
    }

    #[test]
    fn an_event_opens_only_with_its_epochs_key() {
        let base = [7u8; 16];
        let sealed = seal(&base, 3, "👏".as_bytes());
        assert_eq!(open(&base, 3, &sealed).as_deref(), Some("👏".as_bytes()));
        assert_eq!(open(&base, 4, &sealed), None);
        assert_eq!(open(&[8u8; 16], 3, &sealed), None);
        let mut bent = sealed.clone();
        *bent.last_mut().unwrap() ^= 1;
        assert_eq!(open(&base, 3, &bent), None);
        // A fresh nonce every time.
        assert_ne!(seal(&base, 3, b"x"), seal(&base, 3, b"x"));
    }

    /// The browser's `call-events.js` must seal and open exactly this; its
    /// test reads the file. `UPDATE_EVENT_VECTORS=1` rewrites it.
    #[test]
    fn the_browser_vectors_are_what_this_seals() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let base = [0x11u8; 16];
        let rows: Vec<String> = [(7u64, "👍"), (8, "🎉"), (u64::MAX, "hello")]
            .iter()
            .enumerate()
            .map(|(i, (epoch, text))| {
                let nonce = [i as u8 + 1; NONCE_LEN];
                let sealed = seal_with(&base, *epoch, nonce, text.as_bytes());
                assert_eq!(open(&base, *epoch, &sealed).unwrap(), text.as_bytes());
                format!(
                    "    {{\"epoch\": \"{epoch}\", \"plain\": \"{}\", \"sealed\": \"{}\"}}",
                    hex(text.as_bytes()),
                    hex(&sealed)
                )
            })
            .collect();
        // And a guest's key, as a host seals it for the browser to open.
        let guest = GuestKey {
            epoch: 7,
            base_key: base.to_vec(),
            own_index: guest_index(5),
            members: vec![0, 1],
        };
        let secret = [0x22u8; 16];
        let sealed_guest = seal_guest_with(&secret, b"chat", [9; NONCE_LEN], &guest);
        let json = format!(
            "{{\n  \"base_key\": \"{}\",\n  \"events\": [\n{}\n  ],\n  \"guest\": {{\"secret\": \"{}\", \"group\": \"{}\", \"epoch\": \"7\", \"own_index\": {}, \"members\": [0, 1], \"sealed\": \"{}\"}}\n}}\n",
            hex(&base),
            rows.join(",\n"),
            hex(&secret),
            hex(b"chat"),
            guest.own_index,
            hex(&sealed_guest)
        );
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/event-vectors.json");
        if std::env::var_os("UPDATE_EVENT_VECTORS").is_some() {
            std::fs::write(path, &json).unwrap();
        }
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            on_disk, json,
            "rerun with UPDATE_EVENT_VECTORS=1, then the browser test"
        );
    }
}
