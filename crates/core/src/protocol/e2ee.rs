//! End-to-end encrypted chat on the wire: key packages, welcomes, read cursors.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `E2eeSent::refused` for a line sealed in an epoch the chat left long ago:
/// the sending device missed a commit and can never catch up. The server has
/// stopped counting it as holding the chat, so a member device is asked to
/// add it back; a client that reads this drops the chat's keys and waits for
/// that invitation (`encrypted-chat.md`, "A device that fell behind"). A
/// refusal text rather than a new field, so an older client merely shows it.
pub const E2EE_BEHIND: &str =
    "this device fell behind the chat and is being added back; send it again in a moment";

/// One device's key package, as handed out by [`NetMessage::E2eeKeyPackages`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeKeyPackage {
    pub device: Uuid,
    pub bytes: Vec<u8>,
}

/// A member's device missing from a chat ([`NetMessage::E2eeAddDevices`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeNewDevice {
    /// The full address of the account it belongs to.
    pub address: String,
    pub package: E2eeKeyPackage,
}

/// An invitation riding on a commit ([`NetMessage::E2eeSend`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeWelcome {
    /// The devices it is addressed to (the ones whose key packages were used).
    pub devices: Vec<Uuid>,
    /// The full addresses of the accounts those devices belong to, which become
    /// members of the chat for delivery purposes. The server checks each device
    /// belongs to one.
    pub members: Vec<String>,
    pub bytes: Vec<u8>,
}

/// "I have read chat `group` up to and including `after`."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeCursor {
    pub group: Vec<u8>,
    pub after: u64,
}
