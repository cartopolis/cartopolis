//! Sealing and opening call frames: SFrame (RFC 9605) keyed from the chat's MLS
//! group, as RFC 9605 §5.2 describes.
//!
//! Every member computes the same base key for an epoch
//! (`cartopolis_e2ee::Device::call_epoch`), and each sender's frame key is
//! derived from it under a Key ID that packs three things (`MlsKeyId`):
//!
//! - the low bits of the **epoch**, so a receiver knows which base key to use;
//! - the sender's **leaf index**, so it knows who is talking;
//! - a **context** this device drew at random when it joined.
//!
//! The context is not decoration. SFrame's nonce is the key plus a per-sender
//! counter, and the counter lives in memory: a device that drops out and rejoins
//! in the same epoch would start counting from zero again under the same key and
//! reuse nonces, which for AES-GCM gives away the authentication key. A fresh
//! context per [`CallKeys`] is a fresh key, whatever the counter says.
//!
//! What this does **not** protect against: a member of the chat forging a frame
//! as another member. The base key is shared, so anyone who can open a frame can
//! also seal one under someone else's index. The members are the people you
//! chose to call; the forwarder is the party this keeps out.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use sframe::crypto::{Aead, Kdf};
use sframe::frame::{EncryptedFrameView, MediaFrameView, MonotonicCounter};
use sframe::header::KeyId;
use sframe::key::{DecryptionKey, EncryptionKey, KeyStore};
use sframe::mls::{MlsExporter, MlsKeyId, MlsKeyIdBitRange};
use sframe::CipherSuite;

use crate::media::Codec;

/// AES-128-GCM with a 16-byte tag, matching the chat's own MLS suite.
pub const SUITE: CipherSuite = CipherSuite::AesGcm128Sha256;

/// How long a base key must be for [`SUITE`]; pass it to `call_epoch`.
pub const BASE_KEY_LEN: usize = 16;

/// 8 bits of epoch and 16 of leaf index leave 39 for the context, of which a
/// join uses 32. 8 epoch bits tell apart the two epochs a receiver ever holds.
fn bits() -> MlsKeyIdBitRange {
    MlsKeyIdBitRange::new(8, 16)
}

/// Epochs whose keys are kept: the current one and the one before it, for
/// frames sealed just before a membership change and delivered just after.
const KEPT_EPOCHS: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no key for this call yet")]
    NoKey,
    #[error("sframe: {0}")]
    Sframe(#[from] sframe::error::SframeError),
}

/// One epoch of the chat, as the keys need it.
struct Epoch {
    epoch: u64,
    base_key: Vec<u8>,
    own_index: u32,
    members: Vec<u32>,
}

/// Hands sframe the base key it asks for. The label must be RFC 9605's, which
/// is the one `call_epoch` exported under; anything else is refused rather than
/// answered with the wrong secret.
struct Exported<'a>(&'a [u8]);

#[derive(Debug, thiserror::Error)]
#[error("asked for an export this device did not make")]
struct WrongExport;

impl MlsExporter for Exported<'_> {
    type BaseKey = Vec<u8>;
    type Error = WrongExport;

    fn export_secret(
        &self,
        label: &str,
        context: &[u8],
        len: usize,
    ) -> Result<Vec<u8>, WrongExport> {
        if label == "SFrame 1.0 Base Key" && context.is_empty() && len == self.0.len() {
            Ok(self.0.to_vec())
        } else {
            Err(WrongExport)
        }
    }
}

struct Sender {
    epoch: u64,
    key: EncryptionKey,
    counter: MonotonicCounter,
}

/// A device's keys for one call: its own sending key, and the receiving keys of
/// everyone else, derived on first use and dropped with their epoch.
pub struct CallKeys {
    context: u32,
    epochs: VecDeque<Epoch>,
    sender: Option<Sender>,
    receivers: HashMap<KeyId, Arc<DecryptionKey>>,
}

impl Default for CallKeys {
    fn default() -> Self {
        Self::new()
    }
}

/// A frame that opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// The sender's leaf index in the chat, for naming who is talking.
    pub sender: u32,
    pub epoch: u64,
    /// The codec frame, clear prefix and all, exactly as the sender encoded it.
    pub frame: Vec<u8>,
}

impl CallKeys {
    /// Keys for a new join, under a context nobody has used before.
    #[must_use]
    pub fn new() -> Self {
        Self::with_context(uuid::Uuid::new_v4().as_u128() as u32)
    }

    /// Keys under a chosen context: for test vectors, which must come out the
    /// same every run. A real join always takes [`Self::new`].
    #[doc(hidden)]
    #[must_use]
    pub fn with_context(context: u32) -> Self {
        Self {
            context,
            epochs: VecDeque::new(),
            sender: None,
            receivers: HashMap::new(),
        }
    }

    /// The chat moved to a new epoch (or the call just started). Sending
    /// switches to it at once; the previous epoch stays openable.
    pub fn set_epoch(&mut self, epoch: u64, base_key: Vec<u8>, own_index: u32, members: Vec<u32>) {
        if self.epochs.back().is_some_and(|e| e.epoch == epoch) {
            return;
        }
        self.epochs.push_back(Epoch {
            epoch,
            base_key,
            own_index,
            members,
        });
        while self.epochs.len() > KEPT_EPOCHS {
            self.epochs.pop_front();
        }
        let live: Vec<u64> = self.epochs.iter().map(|e| e.epoch & 0xff).collect();
        self.receivers
            .retain(|kid, _| live.contains(&MlsKeyId::from_key_id(*kid, bits()).epoch_lsb()));
        self.sender = None;
    }

    /// Seal one encoded frame for sending.
    pub fn seal(&mut self, codec: Codec, frame: &[u8]) -> Result<Vec<u8>, Error> {
        let current = self.epochs.back().ok_or(Error::NoKey)?;
        if self
            .sender
            .as_ref()
            .is_none_or(|s| s.epoch != current.epoch)
        {
            let kid = MlsKeyId::try_new(self.context, current.epoch, current.own_index, bits())?;
            let key = EncryptionKey::derive_from_mls(SUITE, &Exported(&current.base_key), kid)?;
            self.sender = Some(Sender {
                epoch: current.epoch,
                key,
                counter: MonotonicCounter::default(),
            });
        }
        let sender = self.sender.as_mut().ok_or(Error::NoKey)?;
        let clear = codec.clear_prefix(frame);
        let view = MediaFrameView::try_with_meta_data(
            &mut sender.counter,
            &frame[clear..],
            &frame[..clear],
        )?;
        let sealed = view.encrypt(&sender.key)?;
        Ok(sealed.as_ref().to_vec())
    }

    /// Open one frame as it arrived from the forwarder.
    pub fn open(&mut self, codec: Codec, bytes: &[u8]) -> Result<Opened, Error> {
        let clear = codec.clear_prefix(bytes);
        let view = EncryptedFrameView::try_with_meta_data(&bytes[clear..], &bytes[..clear])?;
        let kid = MlsKeyId::from_key_id(view.header().key_id(), bits());
        let frame = view.decrypt(Receivers {
            epochs: &self.epochs,
            cache: &mut self.receivers,
        })?;
        let epoch = self
            .epochs
            .iter()
            .find(|e| e.epoch & 0xff == kid.epoch_lsb())
            .map_or(0, |e| e.epoch);
        let mut out = frame.meta_data().to_vec();
        out.extend_from_slice(frame.payload());
        Ok(Opened {
            sender: kid.member_index() as u32,
            epoch,
            frame: out,
        })
    }
}

/// The receiving side of [`CallKeys`], in the shape sframe decrypts against.
/// A lookup only *derives*; the key is kept by `record`, which sframe calls
/// only after the frame authenticated — so a forged Key ID cannot fill the
/// cache or evict anything.
struct Receivers<'a> {
    epochs: &'a VecDeque<Epoch>,
    cache: &'a mut HashMap<KeyId, Arc<DecryptionKey>>,
}

#[derive(Debug, thiserror::Error)]
enum Refused {
    #[error("no such epoch")]
    Epoch,
    #[error("not a member of the chat")]
    NotMember,
    #[error("derive: {0}")]
    Derive(String),
}

impl KeyStore<Aead, Kdf> for Receivers<'_> {
    type Key = Arc<DecryptionKey>;
    type Error = Refused;

    fn lookup(&self, key_id: KeyId) -> Result<Self::Key, Self::Error> {
        if let Some(k) = self.cache.get(&key_id) {
            return Ok(k.clone());
        }
        let kid = MlsKeyId::from_key_id(key_id, bits());
        let epoch = self
            .epochs
            .iter()
            .find(|e| e.epoch & 0xff == kid.epoch_lsb())
            .ok_or(Refused::Epoch)?;
        let index = u32::try_from(kid.member_index()).map_err(|_| Refused::NotMember)?;
        // Our own index never arrives: the forwarder does not echo a stream to
        // its sender, so a frame claiming it is somebody replaying ours.
        if index == epoch.own_index || !epoch.members.contains(&index) {
            return Err(Refused::NotMember);
        }
        DecryptionKey::derive_from_mls(SUITE, &Exported(&epoch.base_key), kid)
            .map(Arc::new)
            .map_err(|e| Refused::Derive(e.to_string()))
    }

    fn record(&mut self, key: Self::Key) {
        self.cache.entry(key.key_id()).or_insert(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_e2ee::Device;
    use uuid::Uuid;

    /// Three devices in one chat, each holding call keys from its own
    /// `call_epoch` — the real MLS exporter, not a stand-in.
    fn chat() -> (Vec<Device>, Vec<u8>) {
        let mut alice = Device::new("alice", Uuid::new_v4()).unwrap();
        let mut bob = Device::new("bob", Uuid::new_v4()).unwrap();
        let mut carol = Device::new("carol", Uuid::new_v4()).unwrap();
        let g = alice.create_group().unwrap();
        let mut kps = bob.key_packages(1).unwrap();
        kps.extend(carol.key_packages(1).unwrap());
        let add = alice.add(&g, &kps).unwrap();
        alice.confirm_pending(&g).unwrap();
        bob.join(add.welcome.as_ref().unwrap()).unwrap();
        carol.join(add.welcome.as_ref().unwrap()).unwrap();
        (vec![alice, bob, carol], g)
    }

    fn keys_for(dev: &Device, g: &[u8]) -> CallKeys {
        let mut k = CallKeys::new();
        refresh(&mut k, dev, g);
        k
    }

    fn refresh(k: &mut CallKeys, dev: &Device, g: &[u8]) {
        let e = dev.call_epoch(g, BASE_KEY_LEN).unwrap();
        k.set_epoch(
            e.epoch,
            e.base_key,
            e.own_index,
            e.members.iter().map(|m| m.0).collect(),
        );
    }

    #[test]
    fn a_member_opens_what_another_sealed_and_learns_who_sent_it() {
        let (devs, g) = chat();
        let mut a = keys_for(&devs[0], &g);
        let mut b = keys_for(&devs[1], &g);
        let mut c = keys_for(&devs[2], &g);
        let alice_index = devs[0].call_epoch(&g, BASE_KEY_LEN).unwrap().own_index;

        let voice = b"an opus packet, say".to_vec();
        let sealed = a.seal(Codec::Opus, &voice).unwrap();
        assert!(
            !sealed.windows(voice.len()).any(|w| w == voice),
            "no plaintext on the wire"
        );
        for rx in [&mut b, &mut c] {
            let got = rx.open(Codec::Opus, &sealed).unwrap();
            assert_eq!(got.frame, voice);
            assert_eq!(got.sender, alice_index);
        }
    }

    #[test]
    fn a_vp8_keyframe_header_stays_readable_and_the_rest_does_not() {
        let (devs, g) = chat();
        let mut a = keys_for(&devs[0], &g);
        let mut b = keys_for(&devs[1], &g);
        let mut frame = vec![0x10, 0, 0, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00];
        frame.extend((0..500u32).map(|i| (i * 7) as u8));
        let sealed = a.seal(Codec::Vp8, &frame).unwrap();
        assert_eq!(
            &sealed[..10],
            &frame[..10],
            "forwarder can see the keyframe"
        );
        assert!(!sealed[10..].windows(16).any(|w| w == &frame[100..116]));
        assert_eq!(b.open(Codec::Vp8, &sealed).unwrap().frame, frame);

        // The clear header is authenticated: flip a bit in it and it no longer opens.
        let mut tampered = a.seal(Codec::Vp8, &frame).unwrap();
        tampered[4] ^= 1;
        assert!(b.open(Codec::Vp8, &tampered).is_err());
    }

    #[test]
    fn someone_outside_the_chat_cannot_open_a_frame() {
        let (devs, g) = chat();
        let mut a = keys_for(&devs[0], &g);
        let (others, g2) = chat();
        let mut stranger = keys_for(&others[1], &g2);
        let sealed = a.seal(Codec::Opus, b"hello").unwrap();
        assert!(stranger.open(Codec::Opus, &sealed).is_err());
    }

    #[test]
    fn two_joins_of_one_device_never_share_a_key() {
        let (devs, g) = chat();
        let mut first = keys_for(&devs[0], &g);
        let mut second = keys_for(&devs[0], &g);
        let a = first.seal(Codec::Opus, b"same words").unwrap();
        let b = second.seal(Codec::Opus, b"same words").unwrap();
        // Same epoch, same sender, both counters at zero: only the context
        // tells the keys apart, and it must.
        assert_ne!(a, b);
        let mut bob = keys_for(&devs[1], &g);
        assert_eq!(bob.open(Codec::Opus, &a).unwrap().frame, b"same words");
        assert_eq!(bob.open(Codec::Opus, &b).unwrap().frame, b"same words");
    }

    #[test]
    fn a_frame_from_just_before_a_membership_change_still_opens_after_it() {
        let (mut devs, g) = chat();
        let mut a = keys_for(&devs[0], &g);
        let mut b = keys_for(&devs[1], &g);
        let late = a.seal(Codec::Opus, b"sealed in the old epoch").unwrap();

        // Dave joins: a commit, a new epoch, a new key for everybody.
        let mut dave = Device::new("dave", Uuid::new_v4()).unwrap();
        let add = devs[0].add(&g, &dave.key_packages(1).unwrap()).unwrap();
        devs[0].confirm_pending(&g).unwrap();
        devs[1].receive(&g, &add.message).unwrap();
        dave.join(add.welcome.as_ref().unwrap()).unwrap();
        refresh(&mut a, &devs[0], &g);
        refresh(&mut b, &devs[1], &g);

        assert_eq!(
            b.open(Codec::Opus, &late).unwrap().frame,
            b"sealed in the old epoch"
        );
        let fresh = a.seal(Codec::Opus, b"new epoch").unwrap();
        assert_eq!(b.open(Codec::Opus, &fresh).unwrap().frame, b"new epoch");
        // Dave joined after `late` was sealed and must not be able to open it.
        let mut d = keys_for(&dave, &g);
        assert!(d.open(Codec::Opus, &late).is_err());
        assert_eq!(d.open(Codec::Opus, &fresh).unwrap().frame, b"new epoch");
    }

    /// The frames the browser's copy of this (`crates/cartopolis/web/sframe.js`)
    /// is held to, byte for byte: its own test seals the same frames under the
    /// same keys and must produce these, and opens these back to the frames.
    ///
    /// The file is written by this test and compared on every run, so a change
    /// to the encryption here fails until the file — and so the browser's
    /// test — is updated with it. `UPDATE_SFRAME_VECTORS=1` rewrites it.
    #[test]
    fn the_browser_vectors_are_what_this_seals() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let base_key = [0x11u8; BASE_KEY_LEN];
        let (epoch, context) = (7u64, 0x1234_5678u32);
        let frames: Vec<(u32, Codec, Vec<u8>)> = vec![
            (2, Codec::Opus, (1..=40).collect()),
            (2, Codec::Vp8, [0x10].into_iter().chain(2..=30).collect()), // keyframe
            (2, Codec::Vp8, [0x31].into_iter().chain(2..=20).collect()), // delta
            (
                300,
                Codec::Opus,
                b"a second sender, index past one byte".to_vec(),
            ),
        ];
        let mut senders: HashMap<u32, CallKeys> = HashMap::new();
        let mut rows = Vec::new();
        for (index, codec, frame) in &frames {
            let keys = senders.entry(*index).or_insert_with(|| {
                let mut k = CallKeys::with_context(context);
                k.set_epoch(epoch, base_key.to_vec(), *index, vec![2, 300]);
                k
            });
            let sealed = keys.seal(*codec, frame).unwrap();
            // And it opens, by a receiver in the same epoch.
            let mut rx = CallKeys::with_context(1);
            rx.set_epoch(epoch, base_key.to_vec(), 9, vec![2, 300]);
            assert_eq!(rx.open(*codec, &sealed).unwrap().frame, *frame);
            let codec = match codec {
                Codec::Opus => "opus",
                Codec::Vp8 => "vp8",
            };
            rows.push(format!(
                "    {{\"index\": {index}, \"codec\": \"{codec}\", \"frame\": \"{}\", \"sealed\": \"{}\"}}",
                hex(frame),
                hex(&sealed)
            ));
        }
        let json = format!(
            "{{\n  \"suite\": \"AES_128_GCM_SHA256_128\",\n  \"context\": {context},\n  \"epoch\": {epoch},\n  \"base_key\": \"{}\",\n  \"frames\": [\n{}\n  ]\n}}\n",
            hex(&base_key),
            rows.join(",\n")
        );
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/sframe-vectors.json");
        if std::env::var_os("UPDATE_SFRAME_VECTORS").is_some() {
            std::fs::create_dir_all(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata")).unwrap();
            std::fs::write(path, &json).unwrap();
        }
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            on_disk, json,
            "rerun with UPDATE_SFRAME_VECTORS=1, then the browser test"
        );
    }
}
