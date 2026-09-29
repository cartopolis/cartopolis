//! End-to-end encrypted calls in private chats (`docs/notes/calls.md`).
//!
//! A call is a private chat's members sending media to the simulator over UDP,
//! and the simulator forwarding each stream to the others. Every frame is sealed
//! on the device that captured it, with a key derived from the chat's MLS group
//! (`cartopolis_e2ee::Device::call_epoch`), so the forwarder handles bytes it
//! cannot open — which is also why it may be a plain forwarder and never a mixer.
//!
//! - [`media`]: which codecs a call carries, and how much of each frame stays in
//!   the clear for the forwarder.
//! - `keys` (`client`): sealing and opening frames.
//! - `opus` (`client`): the voice codec.
//! - `sfu` (`sfu`, native): the forwarder the simulator runs.

pub mod media;

#[cfg(feature = "client")]
pub mod keys;
#[cfg(feature = "client")]
pub mod opus;

/// Install str0m's pure-Rust crypto as the process default. Idempotent, and
/// required: str0m does not pick one itself when its C default is compiled out,
/// and fails at the first connection rather than at build time.
#[cfg(not(target_arch = "wasm32"))]
pub fn install_crypto() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| str0m::crypto::from_feature_flags().install_process_default());
}
