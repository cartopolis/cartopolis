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
    /// The video baseline: every Android device has a software encoder for it,
    /// and a browser without a VP9 encoder sends it.
    Vp8,
    /// What this app's own encoder sends (`video`): at the same picture
    /// quality about half VP8's bitrate, and the pure-Rust encoder is faster
    /// than the VP8 one too. Every receiver decodes both.
    Vp9,
}

impl Codec {
    /// How many leading bytes of a frame stay readable to the forwarder.
    ///
    /// - Opus: none. Audio needs no keyframes.
    /// - VP8: the 10-byte keyframe header (frame tag, start code, size) on a
    ///   keyframe, the 3-byte frame tag otherwise. Bit 0 of the first byte is
    ///   the frame type, and it is what str0m reads to call a frame a keyframe.
    /// - VP9: the first 10 bytes of a keyframe's uncompressed header (frame
    ///   marker, profile, frame type, sync code, colour config and size), the
    ///   first byte otherwise. That byte holds the frame type, and it is what
    ///   str0m's VP9 packetizer reads to set the descriptor's keyframe bit.
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
            Codec::Vp9 => match frame.first() {
                Some(_) if self.is_keyframe(frame) => 10,
                Some(_) => 1,
                None => 0,
            },
        };
        n.min(frame.len())
    }

    /// Whether `frame` (plaintext, or sealed — the bytes read are the clear
    /// prefix) starts a picture that decodes on its own.
    #[must_use]
    pub fn is_keyframe(self, frame: &[u8]) -> bool {
        let Some(&b) = frame.first() else {
            return false;
        };
        match self {
            Codec::Opus => true,
            Codec::Vp8 => b & 1 == 0,
            // frame_marker 0b10, then profile_low, profile_high; profiles 0
            // and 1 put show_existing_frame and frame_type at bits 3 and 2,
            // profiles 2 and 3 one bit lower. A shown existing frame is not a
            // keyframe however its type bit reads.
            Codec::Vp9 => {
                if b & 0xc0 != 0x80 {
                    return false;
                }
                let shift = u8::from(b & 0x10 != 0);
                (b >> (3 - shift)) & 1 == 0 && (b >> (2 - shift)) & 1 == 0
            }
        }
    }

    /// Video codecs, as opposed to voice.
    #[must_use]
    pub fn is_video(self) -> bool {
        matches!(self, Codec::Vp8 | Codec::Vp9)
    }
}

/// How long a participant may stay without a working connection before it is
/// taken out of the call.
///
/// str0m's ICE-lite side prunes a candidate pair that has seen no connectivity
/// check for 15 s and reports the connection `Disconnected` — which also
/// happens to a client that simply started late (a phone rendering a slow
/// frame, a join answered over a slow network), and to one that dropped off
/// the network for a moment. A later check from the client brings it back.
/// Treating the first `Disconnected` as the end, as this forwarder first did,
/// turned every late start into a call that failed 25 s in; found by two
/// headless clients on the software renderer, 2026-09-29.
pub const RECONNECT_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

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

    #[test]
    fn vp9_keeps_its_uncompressed_header_start_readable() {
        // Profile 0: 10 0 0 | show_existing 0 | frame_type 0 (key) | show 1 | er 0
        let key = [0b1000_0010, 0x49, 0x83, 0x42, 0, 1, 2, 3, 4, 5, 6, 7];
        // frame_type 1: an inter frame.
        let inter = [0b1000_0110, 1, 2, 3];
        // show_existing_frame: never a keyframe.
        let existing = [0b1000_1000];
        assert!(Codec::Vp9.is_keyframe(&key));
        assert!(!Codec::Vp9.is_keyframe(&inter));
        assert!(!Codec::Vp9.is_keyframe(&existing));
        assert!(
            !Codec::Vp9.is_keyframe(&[0x10]),
            "a VP8 keyframe is not VP9"
        );
        assert_eq!(Codec::Vp9.clear_prefix(&key), 10);
        assert_eq!(Codec::Vp9.clear_prefix(&inter), 1);
        assert_eq!(Codec::Vp9.clear_prefix(&[]), 0);
        // Profile 2 (10-bit): the type bits sit one lower.
        assert!(Codec::Vp9.is_keyframe(&[0b1001_0000]));
        assert!(!Codec::Vp9.is_keyframe(&[0b1001_0010]));
    }
}
