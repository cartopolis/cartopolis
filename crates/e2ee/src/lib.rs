//! End-to-end encrypted private chats (`docs/notes/encrypted-chat.md`).
//!
//! One [`Device`] per installation per account: its signing key, the MLS state of
//! every private chat it is in, and the calls that turn plaintext into sealed
//! bytes and back. It does no I/O. What goes over the wire, and when, is the
//! caller's business — the client's resource sends what this returns and feeds
//! this what arrives — which is what lets the tests below run three devices
//! against each other with a `Vec` standing in for the server.
//!
//! # The server's part, and why it is small
//!
//! The simulator is an MLS *delivery service*: it keeps a directory of key
//! packages (the one-use public keys a device publishes so others can add it to
//! a chat), an ordered log of sealed messages per chat, and a list of which
//! accounts are in which chat so it knows whom to deliver to. It never holds a
//! key that opens anything. Two things follow and both are load-bearing:
//!
//! - **Commits must be applied in one order by everyone**, so the server orders
//!   them: a commit is accepted only if it was made in the chat's current epoch
//!   ([`Outgoing::epoch`]), and the second of two simultaneous commits is
//!   refused. The refused sender calls [`Device::discard_pending`] and tries
//!   again after catching up. Application messages carry no such constraint.
//! - **A device does not read its own entries back.** MLS does not hand a
//!   sender its own application message as a message (openmls answers with a
//!   variant this crate refuses), and its own commit was already applied by
//!   [`Device::confirm_pending`]. The log replays everything to everyone, so a
//!   device skips entries whose sender is itself ([`Device::is_me`]) and shows
//!   what it sent from its own copy.
//!
//! # Identity
//!
//! A member's credential is its full address (`name@server`, protocol 37) and
//! device id, joined by a NUL. The server vouches for the address at sign-in;
//! nothing here verifies it further. That is the known limit of shipping
//! without identity proofs, and the note says so. A full address rather than
//! a username because a credential lasts as long as the device, and chats are
//! meant to reach other servers one day (`cartopolis_core::address`).

use std::collections::BTreeMap;

use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// X25519, AES-128-GCM, SHA-256, Ed25519: MLS's mandatory-to-implement suite,
/// and the one every other implementation is guaranteed to speak.
const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

/// Past epochs whose keys are kept, so a message sent just before a membership
/// change and delivered just after it still opens. Every epoch kept is key
/// material a stolen device would still hold, so it is small.
const MAX_PAST_EPOCHS: usize = 3;

/// Version of [`Device::save`]'s format. Bumped when the layout changes; an
/// older blob is refused rather than half-read, because a device that
/// misreads its own keys is worse than one that starts again.
///
/// 2 (protocol 37): identities are full addresses. A version-1 device's
/// credential holds a bare username for as long as it exists, so it is refused
/// and the client starts a new device.
const SAVE_VERSION: u8 = 2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Anything the MLS library refused, with its own words. Kept as text so
    /// the storage provider's error type does not leak into every caller.
    #[error("{0}")]
    Mls(String),
    #[error("not in that chat")]
    UnknownGroup,
    #[error("unreadable bytes: {0}")]
    Decode(String),
    #[error("saved state is from a different version ({0})")]
    WrongVersion(u8),
}

fn mls(e: impl std::fmt::Debug) -> Error {
    Error::Mls(format!("{e:?}"))
}

/// Who sent something: the account and which of its devices.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identity {
    pub address: String,
    pub device: Uuid,
}

impl Identity {
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.address.as_bytes().to_vec();
        out.push(0);
        out.extend_from_slice(self.device.as_bytes());
        out
    }

    fn from_bytes(b: &[u8]) -> Option<Self> {
        let split = b.iter().position(|&c| c == 0)?;
        let address = String::from_utf8(b[..split].to_vec()).ok()?;
        let device = Uuid::from_slice(b.get(split + 1..)?).ok()?;
        Some(Self { address, device })
    }

    fn from_credential(c: &Credential) -> Option<Self> {
        BasicCredential::try_from(c.clone())
            .ok()
            .and_then(|b| Self::from_bytes(b.identity()))
    }
}

/// What a device hands the server after changing a chat's membership.
#[derive(Debug, Clone)]
pub struct Outgoing {
    /// Group id, as the server keys it.
    pub group: Vec<u8>,
    /// The epoch the message was made in. For a commit the server accepts it
    /// only if this is the chat's current epoch.
    pub epoch: u64,
    /// The sealed message for the chat's log.
    pub message: Vec<u8>,
    /// For a commit that adds devices: the invitation, addressed to them.
    pub welcome: Option<Vec<u8>>,
}

/// What reading one log entry produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// Somebody said something. The bytes are a [`Payload`] (see
    /// [`Payload::decode`]); kept raw here so a newer message type this build
    /// cannot read is still *received* — advancing the ratchet — and only
    /// fails at display.
    Message { from: Identity, bytes: Vec<u8> },
    /// Membership changed; [`Device::members`] has the new list.
    Membership { by: Identity },
}

/// The plaintext inside an application message.
///
/// **Append-only**: postcard encodes a variant by its index, so a new kind goes
/// at the end and nothing is ever reordered or removed. An older build that
/// meets a variant it lacks gets a decode error and shows "a message this
/// version can't display" instead of guessing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Payload {
    Text {
        body: String,
        sent_at_ms: u64,
    },
    /// An ActivityStreams object as JSON — today always a point
    /// (`cartopolis_core::point`). Carried as text rather than as a typed struct
    /// so this crate stays about sealing, and so a newer kind of object reaches
    /// an older build as "something it cannot show" rather than a decode error
    /// that looks like a broken chat.
    Object {
        json: String,
        sent_at_ms: u64,
    },
    /// Where the sender's *phone* is, shared with this chat until `until_ms`
    /// (Unix milliseconds). Sent repeatedly while the share lasts.
    ///
    /// **The end is enforced by the devices, not the server**, which cannot read
    /// this: the sender stops sending at `until_ms`, and every receiver drops the
    /// position at `until_ms` whatever it last heard — so a sender whose phone
    /// dies mid-share still disappears on time. Never federated and never an
    /// ActivityStreams object: a live position is not a post.
    Location {
        latitude: f64,
        longitude: f64,
        accuracy_m: f32,
        until_ms: u64,
        sent_at_ms: u64,
    },
    /// The sender stopped sharing before `until_ms`.
    LocationStopped {
        sent_at_ms: u64,
    },
    /// A group chat's name, set by whoever sent it last. Sealed like
    /// everything else, so the server never learns what a group is called.
    GroupName {
        name: String,
        sent_at_ms: u64,
    },
    /// A picture: where its ciphertext is stored (the simulator's picture
    /// store, by hash) and the key that opens it ([`seal_attachment`]).
    ///
    /// The picture itself is not in here because the server refuses a log
    /// entry over 64 KB, on purpose; the key is, because this message is the
    /// one thing only the chat's members can read. `width`/`height` are the
    /// picture's own, so a bubble can be laid out before it has arrived.
    Picture {
        hash: String,
        key: [u8; 32],
        nonce: [u8; 12],
        width: u32,
        height: u32,
        caption: String,
        sent_at_ms: u64,
    },
}

/// An attachment encrypted for upload, and what opens it.
#[derive(Debug, Clone)]
pub struct SealedAttachment {
    pub bytes: Vec<u8>,
    pub key: [u8; 32],
    pub nonce: [u8; 12],
}

/// Encrypt an attachment under a key made for it alone.
///
/// ChaCha20-Poly1305 through the same RustCrypto provider MLS runs on, so this
/// adds no dependency and no C. A fresh random key per attachment means the
/// nonce can never repeat under one key; it is random too, rather than zero,
/// only so nobody has to re-derive that argument.
pub fn seal_attachment(plain: &[u8]) -> Result<SealedAttachment, Error> {
    let provider = OpenMlsRustCrypto::default();
    let key: [u8; 32] = provider.rand().random_array().map_err(mls)?;
    let nonce: [u8; 12] = provider.rand().random_array().map_err(mls)?;
    let bytes = provider
        .crypto()
        .aead_encrypt(AeadType::ChaCha20Poly1305, &key, plain, &nonce, &[])
        .map_err(mls)?;
    Ok(SealedAttachment { bytes, key, nonce })
}

/// Open an attachment. Fails — never returns garbage — if a byte of it, the
/// key or the nonce is wrong: the tag is checked first.
pub fn open_attachment(sealed: &[u8], key: &[u8; 32], nonce: &[u8; 12]) -> Result<Vec<u8>, Error> {
    OpenMlsRustCrypto::default()
        .crypto()
        .aead_decrypt(AeadType::ChaCha20Poly1305, key, sealed, nonce, &[])
        .map_err(|e| Error::Decode(format!("attachment does not open: {e:?}")))
}

impl Payload {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).unwrap_or_default()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        postcard::from_bytes(bytes).map_err(|e| Error::Decode(e.to_string()))
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u8,
    me: Identity,
    signer_public: Vec<u8>,
    groups: Vec<Vec<u8>>,
    storage: BTreeMap<Vec<u8>, Vec<u8>>,
}

/// One installation's keys and chats.
pub struct Device {
    me: Identity,
    provider: OpenMlsRustCrypto,
    signer: SignatureKeyPair,
    credential: CredentialWithKey,
    groups: BTreeMap<Vec<u8>, MlsGroup>,
}

impl Device {
    /// A new device with a fresh signing key and no chats, for the person at
    /// `address` — a full `name@server`, never a bare username: it goes into
    /// the credential, which cannot change for as long as the device exists.
    pub fn new(address: &str, device: Uuid) -> Result<Self, Error> {
        match address.rsplit_once('@') {
            Some((name, server)) if !name.is_empty() && !server.is_empty() => {}
            _ => return Err(Error::Mls(format!("{address:?} is not a full address"))),
        }
        let provider = OpenMlsRustCrypto::default();
        let signer = SignatureKeyPair::new(CIPHERSUITE.signature_algorithm()).map_err(mls)?;
        signer.store(provider.storage()).map_err(mls)?;
        let me = Identity {
            address: address.to_lowercase(),
            device,
        };
        let credential = credential_for(&me, &signer);
        Ok(Self {
            me,
            provider,
            signer,
            credential,
            groups: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn identity(&self) -> &Identity {
        &self.me
    }

    /// Whether a log entry's sender is this very device.
    #[must_use]
    pub fn is_me(&self, address: &str, device: Uuid) -> bool {
        device == self.me.device && address.eq_ignore_ascii_case(&self.me.address)
    }

    /// Everything needed to come back after a restart, as one blob.
    ///
    /// It holds private keys. The caller stores it where the app keeps its own
    /// data and nowhere else — never in a cache that gets evicted, since losing
    /// it means losing every chat on this device.
    #[must_use]
    pub fn save(&self) -> Vec<u8> {
        let storage = self
            .provider
            .storage()
            .values
            .read()
            .map(|v| v.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let saved = Saved {
            version: SAVE_VERSION,
            me: self.me.clone(),
            signer_public: self.signer.public().to_vec(),
            groups: self.groups.keys().cloned().collect(),
            storage,
        };
        postcard::to_allocvec(&saved).unwrap_or_default()
    }

    pub fn load(bytes: &[u8]) -> Result<Self, Error> {
        let saved: Saved = postcard::from_bytes(bytes).map_err(|e| Error::Decode(e.to_string()))?;
        if saved.version != SAVE_VERSION {
            return Err(Error::WrongVersion(saved.version));
        }
        let provider = OpenMlsRustCrypto::default();
        if let Ok(mut values) = provider.storage().values.write() {
            values.extend(saved.storage);
        }
        let signer = SignatureKeyPair::read(
            provider.storage(),
            &saved.signer_public,
            CIPHERSUITE.signature_algorithm(),
        )
        .ok_or_else(|| Error::Mls("signing key missing from saved state".into()))?;
        let mut groups = BTreeMap::new();
        for id in saved.groups {
            if let Some(g) =
                MlsGroup::load(provider.storage(), &GroupId::from_slice(&id)).map_err(mls)?
            {
                groups.insert(id, g);
            }
        }
        let credential = credential_for(&saved.me, &signer);
        Ok(Self {
            me: saved.me,
            provider,
            signer,
            credential,
            groups,
        })
    }

    /// `n` fresh key packages for the server's directory. Each can be used once,
    /// by one person adding this device to one chat.
    pub fn key_packages(&mut self, n: usize) -> Result<Vec<Vec<u8>>, Error> {
        (0..n)
            .map(|_| {
                let bundle = KeyPackage::builder()
                    .build(
                        CIPHERSUITE,
                        &self.provider,
                        &self.signer,
                        self.credential.clone(),
                    )
                    .map_err(mls)?;
                bundle.key_package().tls_serialize_detached().map_err(mls)
            })
            .collect()
    }

    /// Start a chat with only this device in it. Returns its id.
    ///
    /// `id` is the caller's: it names the chat's home server
    /// (`cartopolis_core::address::group_id`), which is a decision about the
    /// network, not about keys.
    pub fn create_group(&mut self, id: &[u8]) -> Result<Vec<u8>, Error> {
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
            .max_past_epochs(MAX_PAST_EPOCHS)
            .build();
        let group = MlsGroup::new_with_group_id(
            &self.provider,
            &self.signer,
            &config,
            GroupId::from_slice(id),
            self.credential.clone(),
        )
        .map_err(mls)?;
        let id = group.group_id().as_slice().to_vec();
        self.groups.insert(id.clone(), group);
        Ok(id)
    }

    /// Add devices to a chat, from key packages the server handed out.
    ///
    /// The change is *pending* until the server accepts the commit: call
    /// [`Self::confirm_pending`] when it does and [`Self::discard_pending`] when
    /// it does not. Until then this device's view of the chat is unchanged.
    pub fn add(&mut self, group: &[u8], key_packages: &[Vec<u8>]) -> Result<Outgoing, Error> {
        let packages = key_packages
            .iter()
            .map(|b| {
                KeyPackageIn::tls_deserialize(&mut b.as_slice())
                    .map_err(|e| Error::Decode(e.to_string()))?
                    .validate(self.provider.crypto(), ProtocolVersion::Mls10)
                    .map_err(mls)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let g = self.groups.get_mut(group).ok_or(Error::UnknownGroup)?;
        let epoch = g.epoch().as_u64();
        let (commit, welcome, _) = g
            .add_members(&self.provider, &self.signer, &packages)
            .map_err(mls)?;
        Ok(Outgoing {
            group: group.to_vec(),
            epoch,
            message: commit.tls_serialize_detached().map_err(mls)?,
            welcome: Some(welcome.tls_serialize_detached().map_err(mls)?),
        })
    }

    /// The server accepted this device's commit: apply it.
    pub fn confirm_pending(&mut self, group: &[u8]) -> Result<(), Error> {
        let g = self.groups.get_mut(group).ok_or(Error::UnknownGroup)?;
        g.merge_pending_commit(&self.provider).map_err(mls)
    }

    /// The server refused this device's commit (somebody else's won the epoch).
    pub fn discard_pending(&mut self, group: &[u8]) -> Result<(), Error> {
        let g = self.groups.get_mut(group).ok_or(Error::UnknownGroup)?;
        g.clear_pending_commit(self.provider.storage()).map_err(mls)
    }

    /// Seal a message for everyone else in the chat.
    pub fn encrypt(&mut self, group: &[u8], payload: &Payload) -> Result<Outgoing, Error> {
        let g = self.groups.get_mut(group).ok_or(Error::UnknownGroup)?;
        let epoch = g.epoch().as_u64();
        let out = g
            .create_message(&self.provider, &self.signer, &payload.encode())
            .map_err(mls)?;
        Ok(Outgoing {
            group: group.to_vec(),
            epoch,
            message: out.tls_serialize_detached().map_err(mls)?,
            welcome: None,
        })
    }

    /// Join a chat from an invitation. Returns the chat's id.
    pub fn join(&mut self, welcome: &[u8]) -> Result<Vec<u8>, Error> {
        let msg = MlsMessageIn::tls_deserialize(&mut &welcome[..])
            .map_err(|e| Error::Decode(e.to_string()))?;
        let MlsMessageBodyIn::Welcome(welcome) = msg.extract() else {
            return Err(Error::Decode("not an invitation".into()));
        };
        let config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
            .max_past_epochs(MAX_PAST_EPOCHS)
            .build();
        let group = StagedWelcome::new_from_welcome(&self.provider, &config, welcome, None)
            .map_err(mls)?
            .into_group(&self.provider)
            .map_err(mls)?;
        let id = group.group_id().as_slice().to_vec();
        self.groups.insert(id.clone(), group);
        Ok(id)
    }

    /// Read one entry of a chat's log that another device wrote.
    pub fn receive(&mut self, group: &[u8], message: &[u8]) -> Result<Received, Error> {
        let msg = MlsMessageIn::tls_deserialize(&mut &message[..])
            .map_err(|e| Error::Decode(e.to_string()))?;
        let protocol = msg
            .try_into_protocol_message()
            .map_err(|e| Error::Decode(format!("{e:?}")))?;
        let g = self.groups.get_mut(group).ok_or(Error::UnknownGroup)?;
        let processed = g.process_message(&self.provider, protocol).map_err(mls)?;
        let from = Identity::from_credential(processed.credential())
            .ok_or_else(|| Error::Decode("sender has an unreadable identity".into()))?;
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => Ok(Received::Message {
                from,
                bytes: app.into_bytes(),
            }),
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                g.merge_staged_commit(&self.provider, *staged)
                    .map_err(mls)?;
                Ok(Received::Membership { by: from })
            }
            // A bare proposal is not something this client sends, and a device's
            // own entries are skipped before they get here. Either arriving means
            // the caller fed this the wrong thing, which must not be reported as
            // a membership change — it was, once, and that hid exactly this.
            _ => Err(Error::Mls(
                "not a message or a commit this client handles".into(),
            )),
        }
    }

    /// Drop a chat this device created and then abandoned (the server pointed
    /// at an existing one instead), keys and all.
    pub fn forget(&mut self, group: &[u8]) {
        if let Some(mut g) = self.groups.remove(group) {
            // A failed delete is not fatal: the group is gone from this
            // device's list either way, and `load` only restores listed groups.
            let _ = g.delete(self.provider.storage());
        }
    }

    /// The chats this device is in.
    pub fn groups(&self) -> impl Iterator<Item = &Vec<u8>> {
        self.groups.keys()
    }

    /// Who is in a chat, one entry per device.
    #[must_use]
    pub fn members(&self, group: &[u8]) -> Vec<Identity> {
        self.groups
            .get(group)
            .map(|g| {
                g.members()
                    .filter_map(|m| Identity::from_credential(&m.credential))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The chat's current epoch, which the server checks commits against.
    #[must_use]
    pub fn epoch(&self, group: &[u8]) -> Option<u64> {
        self.groups.get(group).map(|g| g.epoch().as_u64())
    }

    /// What a call in this chat needs from its current epoch
    /// (`docs/notes/calls.md`): the SFrame base key every member computes and
    /// nobody else can, and which member sends under which leaf index.
    ///
    /// The key is the MLS exporter with RFC 9605 §5.2's label, so it changes
    /// with every membership change — somebody removed from the chat cannot
    /// hear the call after the commit that removed them. The caller keeps the
    /// previous epoch's answer for a moment, because frames sealed just before
    /// a commit arrive just after it and this device can no longer export for
    /// an epoch it has left.
    pub fn call_epoch(&self, group: &[u8], key_len: usize) -> Result<CallEpoch, Error> {
        let g = self.groups.get(group).ok_or(Error::UnknownGroup)?;
        let base_key = g
            .export_secret(self.provider.crypto(), CALL_KEY_LABEL, b"", key_len)
            .map_err(mls)?;
        Ok(CallEpoch {
            epoch: g.epoch().as_u64(),
            base_key,
            own_index: g.own_leaf_index().u32(),
            members: g
                .members()
                .filter_map(|m| Some((m.index.u32(), Identity::from_credential(&m.credential)?)))
                .collect(),
        })
    }
}

/// RFC 9605 §5.2's exporter label for an SFrame base key.
pub const CALL_KEY_LABEL: &str = "SFrame 1.0 Base Key";

/// One epoch of a chat, as a call sees it ([`Device::call_epoch`]).
#[derive(Clone, PartialEq, Eq)]
pub struct CallEpoch {
    pub epoch: u64,
    /// `MLS-Exporter(CALL_KEY_LABEL, "", key_len)`. Secret: never logged.
    pub base_key: Vec<u8>,
    /// This device's leaf index, which names its frames.
    pub own_index: u32,
    /// Every member device by leaf index, so a frame's sender can be named.
    pub members: Vec<(u32, Identity)>,
}

impl std::fmt::Debug for CallEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallEpoch")
            .field("epoch", &self.epoch)
            .field("own_index", &self.own_index)
            .field("members", &self.members)
            .finish_non_exhaustive()
    }
}

fn credential_for(me: &Identity, signer: &SignatureKeyPair) -> CredentialWithKey {
    CredentialWithKey {
        credential: BasicCredential::new(me.to_bytes()).into(),
        signature_key: signer.public().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_attachment_opens_with_its_key_and_with_nothing_else() {
        let picture = b"\xff\xd8 pretend this is a jpeg".repeat(100);
        let sealed = seal_attachment(&picture).unwrap();
        assert_ne!(
            sealed.bytes[..picture.len()],
            picture[..],
            "it is encrypted"
        );
        assert_eq!(
            open_attachment(&sealed.bytes, &sealed.key, &sealed.nonce).unwrap(),
            picture
        );
        let mut wrong_key = sealed.key;
        wrong_key[0] ^= 1;
        assert!(open_attachment(&sealed.bytes, &wrong_key, &sealed.nonce).is_err());
        let mut tampered = sealed.bytes.clone();
        tampered[3] ^= 1;
        assert!(open_attachment(&tampered, &sealed.key, &sealed.nonce).is_err());
        // Two seals of one picture share nothing.
        let again = seal_attachment(&picture).unwrap();
        assert_ne!(again.key, sealed.key);
        assert_ne!(again.bytes, sealed.bytes);
    }

    #[test]
    fn a_picture_payload_round_trips_and_is_appended_last() {
        let p = Payload::Picture {
            hash: "ab".repeat(32),
            key: [7; 32],
            nonce: [9; 12],
            width: 1600,
            height: 900,
            caption: "the Vismarkt".into(),
            sent_at_ms: 42,
        };
        assert_eq!(Payload::decode(&p.encode()).unwrap(), p);
        // Postcard writes the variant index first: an older build must meet a
        // variant it lacks, never a renumbered one it thinks it knows.
        assert_eq!(p.encode()[0], 5);
    }

    /// The server's chat log, reduced to what the ordering rule needs: entries
    /// in order, and the epoch a commit must have been made in.
    #[derive(Default)]
    struct Log {
        entries: Vec<(Identity, Vec<u8>)>,
        epoch: u64,
    }

    impl Log {
        /// The server's acceptance test for a commit.
        fn commit(&mut self, from: &Identity, out: &Outgoing) -> bool {
            if out.epoch != self.epoch {
                return false;
            }
            self.epoch += 1;
            self.entries.push((from.clone(), out.message.clone()));
            true
        }

        fn send(&mut self, from: &Identity, out: &Outgoing) {
            self.entries.push((from.clone(), out.message.clone()));
        }
    }

    /// Replay the log from `cursor`, skipping this device's own entries.
    fn catch_up(dev: &mut Device, group: &[u8], log: &Log, cursor: &mut usize) -> Vec<Received> {
        let mut got = Vec::new();
        while *cursor < log.entries.len() {
            let (from, bytes) = &log.entries[*cursor];
            *cursor += 1;
            if dev.is_me(&from.address, from.device) {
                continue;
            }
            got.push(dev.receive(group, bytes).expect("entry reads"));
        }
        got
    }

    fn text(got: &[Received]) -> Vec<String> {
        got.iter()
            .filter_map(|r| match r {
                Received::Message { bytes, .. } => match Payload::decode(bytes).ok()? {
                    Payload::Text { body, .. } => Some(body),
                    _ => None,
                },
                Received::Membership { .. } => None,
            })
            .collect()
    }

    /// A fresh chat id whose home is the test server.
    fn test_group() -> Vec<u8> {
        let mut id = b"test.example/".to_vec();
        id.extend_from_slice(Uuid::new_v4().as_bytes());
        id
    }

    fn say(body: &str) -> Payload {
        Payload::Text {
            body: body.into(),
            sent_at_ms: 0,
        }
    }

    #[test]
    fn two_people_talk_and_the_log_holds_only_sealed_bytes() {
        let mut alice = Device::new("alice@test.example", Uuid::new_v4()).unwrap();
        let mut bob = Device::new("bob@test.example", Uuid::new_v4()).unwrap();
        let mut log = Log::default();

        let g = alice.create_group(&test_group()).unwrap();
        let bob_kp = bob.key_packages(1).unwrap();
        let add = alice.add(&g, &bob_kp).unwrap();
        assert!(log.commit(alice.identity(), &add));
        alice.confirm_pending(&g).unwrap();
        let joined = bob.join(add.welcome.as_ref().unwrap()).unwrap();
        assert_eq!(joined, g);
        // Bob joined *after* the commit that added him, so his cursor starts past it.
        let mut bob_cursor = log.entries.len();
        let mut alice_cursor = log.entries.len();

        let secret = "meet at the café at seven";
        let m = alice.encrypt(&g, &say(secret)).unwrap();
        log.send(alice.identity(), &m);
        assert!(
            !m.message
                .windows(secret.len())
                .any(|w| w == secret.as_bytes()),
            "the server's copy must not contain the plaintext"
        );
        assert_eq!(
            text(&catch_up(&mut bob, &g, &log, &mut bob_cursor)),
            vec![secret]
        );

        let r = bob.encrypt(&g, &say("see you there")).unwrap();
        log.send(bob.identity(), &r);
        assert_eq!(
            text(&catch_up(&mut alice, &g, &log, &mut alice_cursor)),
            vec!["see you there"]
        );
    }

    #[test]
    fn every_member_derives_the_same_call_key_and_it_changes_with_the_group() {
        let mut alice = Device::new("alice@test.example", Uuid::new_v4()).unwrap();
        let mut bob = Device::new("bob@test.example", Uuid::new_v4()).unwrap();
        let mut carol = Device::new("carol@test.example", Uuid::new_v4()).unwrap();
        let g = alice.create_group(&test_group()).unwrap();
        let add = alice.add(&g, &bob.key_packages(1).unwrap()).unwrap();
        alice.confirm_pending(&g).unwrap();
        bob.join(add.welcome.as_ref().unwrap()).unwrap();

        let a = alice.call_epoch(&g, 16).unwrap();
        let b = bob.call_epoch(&g, 16).unwrap();
        assert_eq!(a.base_key, b.base_key);
        assert_eq!(a.base_key.len(), 16);
        assert_eq!(a.epoch, b.epoch);
        assert_ne!(a.own_index, b.own_index, "each sender has its own index");
        assert_eq!(a.members, b.members);
        let bob_index = a
            .members
            .iter()
            .find(|(_, id)| id.address == "bob@test.example")
            .map(|(i, _)| *i);
        assert_eq!(bob_index, Some(b.own_index));

        // Carol joins: a new epoch, so a new key Bob gets only by reading the commit.
        let add = alice.add(&g, &carol.key_packages(1).unwrap()).unwrap();
        alice.confirm_pending(&g).unwrap();
        carol.join(add.welcome.as_ref().unwrap()).unwrap();
        bob.receive(&g, &add.message).unwrap();
        let a2 = alice.call_epoch(&g, 16).unwrap();
        assert_ne!(a2.base_key, a.base_key);
        assert_eq!(a2.base_key, bob.call_epoch(&g, 16).unwrap().base_key);
        assert_eq!(a2.base_key, carol.call_epoch(&g, 16).unwrap().base_key);
        assert!(
            format!("{a2:?}").find("base_key").is_none(),
            "never printed"
        );
    }

    #[test]
    fn a_devices_own_entry_never_reads_back_as_a_message() {
        let mut alice = Device::new("alice@test.example", Uuid::new_v4()).unwrap();
        let mut bob = Device::new("bob@test.example", Uuid::new_v4()).unwrap();
        let g = alice.create_group(&test_group()).unwrap();
        let add = alice.add(&g, &bob.key_packages(1).unwrap()).unwrap();
        alice.confirm_pending(&g).unwrap();
        bob.join(add.welcome.as_ref().unwrap()).unwrap();
        let m = alice.encrypt(&g, &say("hi")).unwrap();
        // What the caller must never do is show it as something Bob said, or
        // as a second copy of Alice's own line.
        assert!(!matches!(
            alice.receive(&g, &m.message),
            Ok(Received::Message { .. })
        ));
    }

    #[test]
    fn of_two_simultaneous_commits_the_server_takes_one_and_the_other_retries() {
        let mut alice = Device::new("alice@test.example", Uuid::new_v4()).unwrap();
        let mut bob = Device::new("bob@test.example", Uuid::new_v4()).unwrap();
        let mut carol = Device::new("carol@test.example", Uuid::new_v4()).unwrap();
        let mut dave = Device::new("dave@test.example", Uuid::new_v4()).unwrap();
        let mut log = Log::default();

        let g = alice.create_group(&test_group()).unwrap();
        let add_bob = alice.add(&g, &bob.key_packages(1).unwrap()).unwrap();
        assert!(log.commit(alice.identity(), &add_bob));
        alice.confirm_pending(&g).unwrap();
        bob.join(add_bob.welcome.as_ref().unwrap()).unwrap();
        let mut alice_cursor = log.entries.len();
        let mut bob_cursor = log.entries.len();

        // Both add somebody in the same epoch.
        let by_alice = alice.add(&g, &carol.key_packages(1).unwrap()).unwrap();
        let by_bob = bob.add(&g, &dave.key_packages(1).unwrap()).unwrap();
        assert!(log.commit(alice.identity(), &by_alice));
        assert!(!log.commit(bob.identity(), &by_bob), "same epoch: refused");
        alice.confirm_pending(&g).unwrap();
        carol.join(by_alice.welcome.as_ref().unwrap()).unwrap();

        // Bob drops his, catches up, and tries again in the new epoch.
        bob.discard_pending(&g).unwrap();
        catch_up(&mut bob, &g, &log, &mut bob_cursor);
        let retry = bob.add(&g, &dave.key_packages(1).unwrap()).unwrap();
        assert!(log.commit(bob.identity(), &retry));
        bob.confirm_pending(&g).unwrap();
        dave.join(retry.welcome.as_ref().unwrap()).unwrap();
        catch_up(&mut alice, &g, &log, &mut alice_cursor);

        let names = |d: &Device| {
            let mut v: Vec<String> = d.members(&g).into_iter().map(|i| i.address).collect();
            v.sort();
            v
        };
        assert_eq!(
            names(&alice),
            [
                "alice@test.example",
                "bob@test.example",
                "carol@test.example",
                "dave@test.example"
            ]
        );
        assert_eq!(names(&alice), names(&bob));
        assert_eq!(alice.epoch(&g), bob.epoch(&g));
    }

    #[test]
    fn a_saved_device_comes_back_able_to_read_its_chats() {
        let mut alice = Device::new("alice@test.example", Uuid::new_v4()).unwrap();
        let mut bob = Device::new("bob@test.example", Uuid::new_v4()).unwrap();
        let g = alice.create_group(&test_group()).unwrap();
        let add = alice.add(&g, &bob.key_packages(1).unwrap()).unwrap();
        alice.confirm_pending(&g).unwrap();
        bob.join(add.welcome.as_ref().unwrap()).unwrap();

        let mut bob = Device::load(&bob.save()).unwrap();
        let m = alice.encrypt(&g, &say("still there?")).unwrap();
        let Received::Message { from, bytes } = bob.receive(&g, &m.message).unwrap() else {
            panic!("expected a message");
        };
        assert_eq!(from.address, "alice@test.example");
        assert_eq!(Payload::decode(&bytes).unwrap(), say("still there?"));
        // And it can still speak, with the key it was saved with.
        let r = bob.encrypt(&g, &say("yes")).unwrap();
        assert!(matches!(
            alice.receive(&g, &r.message).unwrap(),
            Received::Message { .. }
        ));
    }

    #[test]
    fn a_key_package_opens_only_one_invitation() {
        let mut alice = Device::new("alice@test.example", Uuid::new_v4()).unwrap();
        let mut bob = Device::new("bob@test.example", Uuid::new_v4()).unwrap();
        let kp = bob.key_packages(1).unwrap();
        let g1 = alice.create_group(&test_group()).unwrap();
        let a1 = alice.add(&g1, &kp).unwrap();
        alice.confirm_pending(&g1).unwrap();
        let g2 = alice.create_group(&test_group()).unwrap();
        let a2 = alice.add(&g2, &kp).unwrap();
        alice.confirm_pending(&g2).unwrap();
        bob.join(a1.welcome.as_ref().unwrap()).unwrap();
        // The private half was consumed by the first join: this is why the
        // server hands each package out once and devices keep a stock of them.
        assert!(bob.join(a2.welcome.as_ref().unwrap()).is_err());
    }

    /// A bare username in a credential could never be told apart from the same
    /// name on another server, and a credential cannot be changed later.
    #[test]
    fn a_device_belongs_to_a_full_address() {
        assert!(Device::new("jeroen", Uuid::new_v4()).is_err());
        assert!(Device::new("@cartopolis.org", Uuid::new_v4()).is_err());
        let d = Device::new("Jeroen@Cartopolis.org", Uuid::new_v4()).unwrap();
        assert_eq!(d.identity().address, "jeroen@cartopolis.org");
    }

    #[test]
    fn identities_survive_the_credential_round_trip() {
        let me = Identity {
            address: "jeroen@cartopolis.org".into(),
            device: Uuid::new_v4(),
        };
        assert_eq!(Identity::from_bytes(&me.to_bytes()), Some(me));
        assert_eq!(Identity::from_bytes(b"no separator"), None);
    }
}
