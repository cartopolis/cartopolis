//! Wire messages (ADR-009). TCP framing is length-delimited (ADR-008); each frame body is an **app frame**:
//! `protocol_version` (u16 LE) + `message_kind` (u16 LE) + `request_id` (u32 LE) + postcard(`NetMessage`).
//!
//! One topic per file, every item re-exported here, so a type's path is
//! `protocol::X` whichever file holds it:
//!
//! - `history.rs` — `PROTOCOL_HISTORY`, the changelog `PROTOCOL_VERSION` is derived from
//! - `kind.rs` — `MessageKind`, the header's number for each message
//! - `message.rs` — `NetMessage` itself
//! - `frame.rs` — encoding and decoding app frames
//! - `avatar.rs`, `prims.rs`, `communities.rs`, `calls.rs`, `contacts.rs`,
//!   `e2ee.rs`, `reports.rs` — the data types messages carry
//! - `wire_shape.rs` / `wire_shape.txt` — the payload's shape, pinned

mod avatar;
mod calls;
mod communities;
mod contacts;
mod e2ee;
mod frame;
mod history;
mod kind;
mod message;
pub mod msg;
mod prims;
mod reports;

pub use avatar::*;
pub use calls::*;
pub use communities::*;
pub use contacts::*;
pub use e2ee::*;
pub use frame::*;
pub use history::*;
pub use kind::*;
pub use message::*;
pub use prims::*;
pub use reports::*;

#[cfg(test)]
mod wire_shape;

#[cfg(test)]
mod tests;
