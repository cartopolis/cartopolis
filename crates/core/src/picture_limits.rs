//! What the picture store accepts (`docs/notes/pictures-in-private-chats.md`).
//! Here rather than in the simulator, which enforces them, for the reason
//! `model_limits` gives: the client shrinks a picture to fit before it sends.

/// The largest stored picture, in bytes — of *ciphertext*, which is the
/// picture plus a 16-byte tag. The client re-encodes to a 1600 px JPEG
/// first (`utils::photo::prepare`), which lands well under this; the ceiling
/// is what keeps the store from being an anonymous file host for anything
/// else that fits in a body.
pub const MAX_PICTURE_BYTES: usize = 4 * 1024 * 1024;

/// How many pictures one account may store in an hour. A conversation sends
/// a handful; this is the number past which it is not a conversation.
pub const MAX_PICTURES_PER_HOUR: usize = 120;
