//! RTCM 3 framing — only enough to *see* the corrections, not to decode them.
//!
//! The bytes from the caster go to the receiver untouched; the receiver is the
//! one that solves with them. What the client needs is to know they are real:
//! a mistyped mountpoint answers with a sourcetable, a wrong password with an
//! HTML page, and a base station that went quiet with nothing at all, and every
//! one of those looks like "connected" from the socket's point of view. So the
//! stream is framed here (preamble `0xD3`, a 10-bit length, CRC-24Q) and the
//! message numbers counted: 1005 is the base's position, 1074/1084/1094/1124 are
//! its GPS/GLONASS/Galileo/BeiDou observations, and a stream that carries those
//! is one an RTK rover can fix on.
//!
//! **The framer is also the receiver's firewall.** A Unicore receiver takes
//! text commands on the same port as corrections — `FRESET` (factory reset),
//! `SAVECONFIG`, a baud change — so a caster that answered with a page of text,
//! or one that meant harm, would be typing at the receiver if its bytes were
//! passed on as they came. Only whole, CRC-checked frames of a standard message
//! number ([`forwardable`]) ever reach the port.

const PREAMBLE: u8 = 0xD3;
/// Longest payload the 10-bit length can describe.
const MAX_PAYLOAD: usize = 1023;

/// Finds RTCM 3 frames in a byte stream.
#[derive(Default)]
pub struct Framer {
    buf: Vec<u8>,
    /// Frames whose CRC did not match.
    pub bad_frames: u64,
}

impl Framer {
    /// Feed bytes; `out` gets each valid frame's message number and the whole
    /// frame (preamble to CRC). Bytes outside a valid frame are dropped.
    pub fn push(&mut self, bytes: &[u8], mut out: impl FnMut(u16, &[u8])) {
        self.buf.extend_from_slice(bytes);
        let mut at = 0;
        loop {
            // Resynchronise on the next preamble.
            match self.buf[at..].iter().position(|&b| b == PREAMBLE) {
                Some(p) => at += p,
                None => {
                    at = self.buf.len();
                    break;
                }
            }
            if self.buf.len() < at + 3 {
                break;
            }
            let len = (usize::from(self.buf[at + 1] & 0x03) << 8) | usize::from(self.buf[at + 2]);
            if self.buf[at + 1] & 0xFC != 0 || len > MAX_PAYLOAD {
                at += 1;
                continue;
            }
            let total = 3 + len + 3;
            if self.buf.len() < at + total {
                break;
            }
            let frame = &self.buf[at..at + total];
            let crc = (u32::from(frame[total - 3]) << 16)
                | (u32::from(frame[total - 2]) << 8)
                | u32::from(frame[total - 1]);
            if crc24q(&frame[..total - 3]) == crc && len >= 2 {
                out((u16::from(frame[3]) << 4) | u16::from(frame[4] >> 4), frame);
                at += total;
            } else {
                self.bad_frames += 1;
                at += 1;
            }
        }
        self.buf.drain(..at);
    }
}

/// Whether a message number is one to hand to the receiver: RTCM 3's standard
/// range. Station and antenna descriptions (1005–1008, 1033), observations
/// (1001–1004, 1009–1012, the MSM 1071–1137), ephemerides and biases (1019–
/// 1046, 1230) all sit in it. The proprietary block (4001–4095) is each
/// vendor's own, undocumented here, and is not passed on.
#[must_use]
pub fn forwardable(message: u16) -> bool {
    (1001..=1299).contains(&message)
}

/// The message number of `bytes` if they are exactly one valid frame of a
/// [`forwardable`] message — the check made again at the port, so nothing
/// that skipped the framer can be written there either.
#[must_use]
pub fn checked_frame(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 8 || bytes[0] != PREAMBLE || bytes[1] & 0xFC != 0 {
        return None;
    }
    let len = (usize::from(bytes[1] & 0x03) << 8) | usize::from(bytes[2]);
    if bytes.len() != 3 + len + 3 || len < 2 {
        return None;
    }
    let crc = (u32::from(bytes[len + 3]) << 16)
        | (u32::from(bytes[len + 4]) << 8)
        | u32::from(bytes[len + 5]);
    if crc24q(&bytes[..len + 3]) != crc {
        return None;
    }
    let message = (u16::from(bytes[3]) << 4) | u16::from(bytes[4] >> 4);
    forwardable(message).then_some(message)
}

/// CRC-24Q, the checksum RTCM 3 (and Qualcomm) use.
#[must_use]
pub fn crc24q(data: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for &b in data {
        crc ^= u32::from(b) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x0100_0000 != 0 {
                crc ^= 0x0186_4CFB;
            }
        }
    }
    crc & 0x00FF_FFFF
}

#[cfg(test)]
pub(crate) fn frame(message: u16, extra: &[u8]) -> Vec<u8> {
    let mut payload = vec![(message >> 4) as u8, ((message & 0x0F) << 4) as u8];
    payload.extend_from_slice(extra);
    let mut f = vec![PREAMBLE, (payload.len() >> 8) as u8, payload.len() as u8];
    f.extend_from_slice(&payload);
    let crc = crc24q(&f);
    f.extend_from_slice(&[(crc >> 16) as u8, (crc >> 8) as u8, crc as u8]);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crc_matches_the_published_check_value() {
        // CRC-24Q of "123456789".
        assert_eq!(crc24q(b"123456789"), 0x00CD_E703);
    }

    #[test]
    fn frames_are_found_through_noise_and_split_reads() {
        let mut stream = b"ICY junk \xD3\x00".to_vec();
        stream.extend(frame(1005, &[0; 17]));
        stream.extend(frame(1077, &[7; 200]));
        let mut f = Framer::default();
        let mut seen = Vec::new();
        for chunk in stream.chunks(5) {
            f.push(chunk, |m, _| seen.push(m));
        }
        assert_eq!(seen, vec![1005, 1077]);
    }

    #[test]
    fn a_corrupted_frame_is_counted_and_skipped() {
        let mut bad = frame(1005, &[0; 17]);
        bad[6] ^= 0xFF;
        let mut stream = bad;
        stream.extend(frame(1094, &[1; 30]));
        let mut f = Framer::default();
        let mut seen = Vec::new();
        f.push(&stream, |m, _| seen.push(m));
        assert_eq!(seen, vec![1094]);
        assert_eq!(f.bad_frames, 1);
    }

    #[test]
    fn only_standard_messages_are_forwardable() {
        for m in [1005, 1006, 1033, 1077, 1087, 1097, 1127, 1230] {
            assert!(forwardable(m), "{m}");
        }
        for m in [0, 999, 1300, 4001, 4094, 4095] {
            assert!(!forwardable(m), "{m}");
        }
    }

    #[test]
    fn checked_frame_takes_one_whole_valid_frame_and_nothing_else() {
        let good = frame(1077, &[3; 40]);
        assert_eq!(checked_frame(&good), Some(1077));
        assert_eq!(checked_frame(&frame(4094, &[0; 10])), None, "proprietary");
        let mut corrupt = good.clone();
        corrupt[10] ^= 1;
        assert_eq!(checked_frame(&corrupt), None);
        let mut two = good.clone();
        two.extend(frame(1005, &[0; 17]));
        assert_eq!(checked_frame(&two), None, "exactly one frame");
        assert_eq!(checked_frame(b"FRESET\r\n"), None);
        assert_eq!(checked_frame(&good[..good.len() - 1]), None);
    }
}
