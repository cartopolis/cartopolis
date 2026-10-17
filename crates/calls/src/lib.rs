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
//! - `clip` (`client`, native): a WebM clip played on a prim.
//! - `keys` (`client`): sealing and opening frames.
//! - `events` (`client`, native): sealing a reaction the server relays unread.
//! - `opus` (`client`): the voice codec.
//! - `video` (`client`): the video codec, and a stand-in camera for tests.
//! - `link` (`client`, native): a device's connection to the forwarder.
//! - `session` (`client`, native): a device's call, on a thread of its own.
//! - `sfu` (`sfu`, native): the forwarder the simulator runs.

#[cfg(not(target_arch = "wasm32"))]
mod crypto;
pub mod media;

#[cfg(all(feature = "client", not(target_arch = "wasm32")))]
pub mod clip;
#[cfg(all(feature = "client", not(target_arch = "wasm32")))]
pub mod events;
#[cfg(feature = "client")]
pub mod keys;
#[cfg(all(feature = "client", not(target_arch = "wasm32")))]
pub mod link;
#[cfg(feature = "client")]
pub mod opus;
#[cfg(all(feature = "client", not(target_arch = "wasm32")))]
pub mod session;
#[cfg(all(feature = "sfu", not(target_arch = "wasm32")))]
pub mod sfu;
#[cfg(feature = "client")]
pub mod video;

/// Install the pure-Rust crypto provider (`crypto`) as str0m's process
/// default. Idempotent, and required: str0m picks none itself when its C
/// default is compiled out, and fails at the first connection rather than at
/// build time.
#[cfg(not(target_arch = "wasm32"))]
pub fn install_crypto() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| crypto::provider().install_process_default());
}
