//! `systems::net` — one file per concern; see systems/mod.rs for the map.

/// The group's registration: its resources and its systems, each in its
/// `crate::phase::ClientPhase`. Added by `run_client`.
mod plugin;
pub use plugin::NetPlugin;

/// Keeping the connection alive while the app is off screen (Android's
/// foreground service).
pub mod background;
/// The sounds of a call: ringtone, ringback, call waiting, join and leave cues.
pub mod call_tones;
/// Private chats: this installation's encryption keys and the conversations
/// they open (protocol 25).
pub mod calls;
/// Pictures in private chats: encrypted, stored on the server, opened here.
pub mod chat_pictures;
pub mod network;
pub mod private_chat;
/// Where the simulator wakes this phone when the app is closed (UnifiedPush).
#[cfg(target_os = "android")]
pub mod push;
pub mod session;
pub mod user_store;
/// Proximity voice: capture, the codec, and the playback of what arrives.
pub mod voice;
/// The browser's carrier for the same wire `network` speaks over TCP. Only the
/// web build has one, and only the web build compiles it.
#[cfg(target_arch = "wasm32")]
pub mod ws_transport;
