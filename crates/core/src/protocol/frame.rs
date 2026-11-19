//! Framing: a message's postcard body and the 8-byte app header before it.

use super::*;
use crate::error::ProtocolError;

pub(super) const APP_HEADER_LEN: usize = 8;

/// Raw postcard body (no app header). Prefer [`encode_app_frame`] / [`decode_app_frame`] on the wire.
#[must_use]
pub fn encode_message(msg: &NetMessage) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(msg)
}

#[must_use]
pub fn decode_message(bytes: &[u8]) -> Result<NetMessage, postcard::Error> {
    postcard::from_bytes(bytes)
}

/// Full ADR-009 app frame: version + kind + request_id + postcard payload.
#[must_use]
pub fn encode_app_frame(msg: &NetMessage) -> Result<Vec<u8>, postcard::Error> {
    let kind = message_kind(msg) as u16;
    let rid = message_request_id(msg);
    let mut out = Vec::with_capacity(APP_HEADER_LEN + 64);
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&rid.to_le_bytes());
    // Serialised straight after the header, not into a second buffer that is
    // then copied: a full snapshot is the size of the world.
    postcard::to_extend(msg, out)
}

/// One avatar's state as the bytes it occupies inside a `WorldSnapshot`'s
/// `avatars` list — for the sim, which encodes each avatar once a tick and then
/// assembles a snapshot per connection from the states within its reach
/// ([`snapshot_frame`]), instead of encoding a list per client.
///
/// Postcard frames nothing: a `Vec<T>` is its length as a varint followed by the
/// elements back to back, and an element's bytes are the same whether it is
/// encoded alone or in a list. `snapshot_frame_matches_encode_app_frame` pins
/// that byte for byte, so this cannot drift from [`encode_app_frame`].
#[must_use]
pub fn encode_avatar_state(state: &AvatarStateDto) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(state)
}

/// A `WorldSnapshot` app frame assembled from states already encoded by
/// [`encode_avatar_state`]: byte-identical to [`encode_app_frame`] of the same
/// snapshot.
#[must_use]
pub fn snapshot_frame<'a>(
    tick: u64,
    states: impl ExactSizeIterator<Item = &'a [u8]>,
) -> Result<Vec<u8>, postcard::Error> {
    // The frame of an empty snapshot ends in its list's length — zero, one
    // byte — which is swapped for the real count.
    let mut out = encode_app_frame(&NetMessage::WorldSnapshot {
        tick,
        avatars: Vec::new(),
    })?;
    debug_assert_eq!(out.last(), Some(&0));
    out.pop();
    // Postcard's varint: LEB128, low seven bits first, the high bit a continuation.
    let mut n = states.len();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    for s in states {
        out.extend_from_slice(s);
    }
    Ok(out)
}

/// The kind in an encoded frame's header, without decoding its payload.
///
/// For the one caller that has to *route* a frame it is not going to read:
/// `net::handle_connection` fans out already-encoded bytes from a broadcast
/// channel carrying snapshots, prim upserts and capacity updates together, and an
/// idle client (`SetPresence`) must drop only the snapshots. Decoding a full
/// `WorldSnapshot` — every region, prim and avatar — per connection per tick, to
/// then throw it away, is the obvious way to write that and is far more expensive
/// than the send it saves.
///
/// Reads the header only, so an unknown kind or a truncated frame is `None`
/// rather than an error: a caller that cannot classify a frame should pass it
/// along and let the real decoder produce the diagnosis.
#[must_use]
pub fn frame_kind(bytes: &[u8]) -> Option<MessageKind> {
    if bytes.len() < APP_HEADER_LEN {
        return None;
    }
    MessageKind::from_wire(u16::from_le_bytes(bytes[2..4].try_into().unwrap()))
}

#[must_use]
pub fn decode_app_frame(bytes: &[u8]) -> Result<NetMessage, ProtocolError> {
    if bytes.len() < APP_HEADER_LEN {
        return Err(ProtocolError::FrameTooShort(bytes.len()));
    }
    let ver = u16::from_le_bytes(bytes[0..2].try_into().unwrap());
    if ver != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(ver));
    }
    let kind_wire = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
    let Some(kind) = MessageKind::from_wire(kind_wire) else {
        return Err(ProtocolError::UnknownMessageKind(kind_wire));
    };
    let _request_id = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let msg = decode_message(&bytes[APP_HEADER_LEN..])?;
    if message_kind(&msg) != kind {
        return Err(ProtocolError::KindMismatch {
            header: kind_wire,
            body: message_kind(&msg) as u16,
        });
    }
    Ok(msg)
}
