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

#[cfg(test)]
mod tests {
    use super::*;

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
        let json = format!(
            "{{\n  \"base_key\": \"{}\",\n  \"events\": [\n{}\n  ]\n}}\n",
            hex(&base),
            rows.join(",\n")
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
