//! The codecs a call carries, and what of each frame the forwarder may read.
//!
//! The forwarder never decodes anything, but it has to answer one question
//! about video: *is this a keyframe?* A receiver that joins late or loses
//! packets asks for one, and the forwarder can only pass that request to the
//! right sender and know when it has been answered if it can see keyframes go
//! by. So a codec's payload header stays in the clear — authenticated as SFrame
//! metadata, never encrypted — and nothing after it. Jitsi's end-to-end mode
//! draws the same line at the same byte counts.

/// A codec a call can carry. Deliberately short: every participant must decode
/// what every other sends, because the forwarder does not transcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    /// Voice, 48 kHz mono.
    Opus,
    /// The video baseline: every Android device has a software encoder for it.
    Vp8,
}

impl Codec {
    /// How many leading bytes of a frame stay readable to the forwarder.
    ///
    /// - Opus: none. Audio needs no keyframes.
    /// - VP8: the 10-byte keyframe header (frame tag, start code, size) on a
    ///   keyframe, the 3-byte frame tag otherwise. Bit 0 of the first byte is
    ///   the frame type, and it is what str0m reads to call a frame a keyframe.
    ///
    /// Clamped to the frame, so a runt frame is simply all header.
    #[must_use]
    pub fn clear_prefix(self, frame: &[u8]) -> usize {
        let n = match self {
            Codec::Opus => 0,
            Codec::Vp8 => match frame.first() {
                Some(b) if b & 1 == 0 => 10,
                Some(_) => 3,
                None => 0,
            },
        };
        n.min(frame.len())
    }
}

/// A participant in a call, as the forwarder numbers them. Opaque to clients,
/// which learn who it is from the simulator's signalling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParticipantId(pub u64);

/// What kind of track a [`TrackInfo`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Audio,
    Video,
}

/// Which participant a track on somebody's *down* connection comes from. Sent
/// with every offer, because a receiver cannot tell from the media itself whose
/// camera a tile is until the first frame opens (the SFrame Key ID names the
/// sender, but only once there is a frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    pub mid: String,
    pub from: ParticipantId,
    pub kind: Kind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vp8_keeps_its_frame_tag_readable_and_nothing_more() {
        let key = [0x10, 1, 2, 0x9d, 0x01, 0x2a, 3, 4, 5, 6, 7, 8];
        let delta = [0x11, 1, 2, 3, 4];
        assert_eq!(Codec::Vp8.clear_prefix(&key), 10);
        assert_eq!(Codec::Vp8.clear_prefix(&delta), 3);
        assert_eq!(Codec::Vp8.clear_prefix(&[0x10, 1]), 2, "runt: all header");
        assert_eq!(Codec::Vp8.clear_prefix(&[]), 0);
        assert_eq!(Codec::Opus.clear_prefix(&key), 0);
    }
}
