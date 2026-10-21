//! Writing a clip: VP8 pictures and Opus sound in a WebM file, the format a
//! prim plays (`clip`, `docs/notes/video-on-a-prim.md`) and a phone's gallery
//! shows. The camera's Video records through this (`docs/notes/camera.md`).
//!
//! The whole clip is held in memory and laid out when it is finished, so every
//! element's size is known and nothing is patched afterwards: a recording is
//! capped at [`crate::video::MAX_PIXELS`]-sized pictures and a few minutes,
//! tens of megabytes at most. No `Cues`: the player indexes a clip by walking
//! it once (`clip::index`), and so do the galleries for a file this size.
//!
//! Frames are added as they come, video from the encoder's thread and sound
//! from the app's, each with its own millisecond clock from the recording's
//! start; they are put in time order when written.

/// A frame waiting to be written.
struct Block {
    track: u8,
    ms: u64,
    key: bool,
    data: Vec<u8>,
}

/// The video track's number, and the sound's.
const VIDEO: u8 = 1;
const AUDIO: u8 = 2;

/// The longest a cluster may run: a block's time is a signed 16-bit offset
/// from its cluster's, in milliseconds.
const CLUSTER_MS: u64 = 30_000;

/// A clip being put together.
pub struct Writer {
    width: u32,
    height: u32,
    /// The sound's input rate, when the clip has sound.
    sound_rate: Option<u32>,
    /// `(name, value)` pairs written as the clip's tags.
    tags: Vec<(String, String)>,
    blocks: Vec<Block>,
    bytes: usize,
}

impl Writer {
    /// A clip of `width`×`height` VP8 pictures and, with `sound_rate`, mono
    /// Opus sound encoded from that rate.
    #[must_use]
    pub fn new(width: u32, height: u32, sound_rate: Option<u32>) -> Self {
        Self {
            width,
            height,
            sound_rate,
            tags: Vec::new(),
            blocks: Vec::new(),
            bytes: 0,
        }
    }

    /// The pictures' size, when it is decided after the first sound came.
    pub fn set_size(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
    }

    /// A tag on the whole clip: what the camera writes the pose into.
    pub fn tag(&mut self, name: &str, value: &str) {
        self.tags.push((name.to_string(), value.to_string()));
    }

    /// One VP8 picture shown at `ms`. Whether it is a keyframe is read from
    /// the frame itself (the low bit of its first byte is clear on one).
    pub fn video(&mut self, ms: u64, frame: Vec<u8>) {
        let key = frame.first().is_some_and(|b| b & 1 == 0);
        self.push(VIDEO, ms, key, frame);
    }

    /// One Opus packet starting at `ms`. Ignored by a clip with no sound.
    pub fn audio(&mut self, ms: u64, packet: Vec<u8>) {
        if self.sound_rate.is_some() {
            self.push(AUDIO, ms, true, packet);
        }
    }

    fn push(&mut self, track: u8, ms: u64, key: bool, data: Vec<u8>) {
        self.bytes += data.len();
        self.blocks.push(Block {
            track,
            ms,
            key,
            data,
        });
    }

    /// The bytes of frames added so far: about the size of the file.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes
    }

    /// Whether no frame has been added.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The clip's length so far, in milliseconds: its last picture's time.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.blocks
            .iter()
            .filter(|b| b.track == VIDEO)
            .map(|b| b.ms)
            .max()
            .unwrap_or(0)
    }

    /// The file. Everything before the first video keyframe is dropped: a
    /// player cannot show a picture before one, and the sound would run ahead.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        self.blocks.sort_by_key(|b| (b.ms, b.track));
        let start = self
            .blocks
            .iter()
            .find(|b| b.track == VIDEO && b.key)
            .map(|b| b.ms);
        let Some(start) = start else {
            self.blocks.clear();
            return self.layout(0);
        };
        self.blocks.retain(|b| b.ms >= start);
        for b in &mut self.blocks {
            b.ms -= start;
        }
        let duration = self.duration_ms();
        self.layout(duration)
    }

    fn layout(&self, duration_ms: u64) -> Vec<u8> {
        let header = element(
            0x1A45_DFA3,
            &[
                uint(0x4286, 1),
                uint(0x42F7, 1),
                uint(0x42F2, 4),
                uint(0x42F3, 8),
                string(0x4282, "webm"),
                uint(0x4287, 4),
                uint(0x4285, 2),
            ]
            .concat(),
        );
        let info = element(
            0x1549_A966,
            &[
                uint(0x2A_D7B1, 1_000_000),
                float(0x4489, duration_ms as f64),
                string(0x4D80, "cartopolis"),
                string(0x5741, "cartopolis"),
            ]
            .concat(),
        );
        let mut tracks = vec![element(
            0xAE,
            &[
                uint(0xD7, u64::from(VIDEO)),
                uint(0x73C5, u64::from(VIDEO)),
                uint(0x83, 1),
                uint(0x9C, 0),
                string(0x86, "V_VP8"),
                element(
                    0xE0,
                    &[
                        uint(0xB0, u64::from(self.width)),
                        uint(0xBA, u64::from(self.height)),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        )];
        if let Some(rate) = self.sound_rate {
            tracks.push(element(
                0xAE,
                &[
                    uint(0xD7, u64::from(AUDIO)),
                    uint(0x73C5, u64::from(AUDIO)),
                    uint(0x83, 2),
                    uint(0x9C, 0),
                    string(0x86, "A_OPUS"),
                    element(0x63A2, &opus_head(rate)),
                    // Opus is always decoded at 48 kHz, whatever went in.
                    element(0xE1, &[float(0xB5, 48_000.0), uint(0x9F, 1)].concat()),
                ]
                .concat(),
            ));
        }
        let tracks = element(0x1654_AE6B, &tracks.concat());
        let tags = if self.tags.is_empty() {
            Vec::new()
        } else {
            let simple: Vec<u8> = self
                .tags
                .iter()
                .flat_map(|(n, v)| {
                    element(0x67C8, &[string(0x45A3, n), string(0x4487, v)].concat())
                })
                .collect();
            element(
                0x1254_C367,
                &element(0x7373, &[element(0x63C0, &[]), simple].concat()),
            )
        };

        let mut clusters = Vec::new();
        let mut cluster: Vec<u8> = Vec::new();
        let mut cluster_ms = 0;
        for b in &self.blocks {
            let new =
                cluster.is_empty() || b.ms - cluster_ms > CLUSTER_MS || (b.track == VIDEO && b.key);
            if new {
                if !cluster.is_empty() {
                    clusters.push(element(0x1F43_B675, &cluster));
                }
                cluster_ms = b.ms;
                cluster = uint(0xE7, cluster_ms);
            }
            // SimpleBlock: the track as a one-byte size, the time from the
            // cluster's, then the flags (keyframe, no lacing).
            let mut block = Vec::with_capacity(b.data.len() + 4);
            block.push(0x80 | b.track);
            block.extend_from_slice(&((b.ms - cluster_ms) as i16).to_be_bytes());
            block.push(if b.key { 0x80 } else { 0 });
            block.extend_from_slice(&b.data);
            cluster.extend(element(0xA3, &block));
        }
        if !cluster.is_empty() {
            clusters.push(element(0x1F43_B675, &cluster));
        }

        let segment = element(
            0x1853_8067,
            &[info, tracks, tags, clusters.concat()].concat(),
        );
        [header, segment].concat()
    }
}

/// The Opus identification header a Matroska Opus track carries: version 1,
/// mono, the encoder's look-ahead to skip, the rate it was fed, no gain, the
/// simple channel mapping.
fn opus_head(rate: u32) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.push(1);
    h.push(1);
    // 6.5 ms of look-ahead at 48 kHz: what an Opus encoder delays by.
    h.extend_from_slice(&312u16.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes());
    h.push(0);
    h
}

/// An element: its ID (already carrying its length marker), its size as the
/// shortest variable-length integer that holds it, its body.
fn element(id: u32, body: &[u8]) -> Vec<u8> {
    let id_bytes = id.to_be_bytes();
    let skip = id_bytes.iter().take_while(|&&b| b == 0).count();
    let mut out = id_bytes[skip..].to_vec();
    out.extend(size(body.len() as u64));
    out.extend_from_slice(body);
    out
}

/// A size as an EBML variable-length integer. All ones is "unknown", so a
/// length of exactly that takes a byte more.
fn size(n: u64) -> Vec<u8> {
    let len = (1..=8u32)
        .find(|&len| n < (1u64 << (7 * len)) - 1)
        .unwrap_or(8);
    let marked = n | (1u64 << (7 * len));
    marked.to_be_bytes()[(8 - len) as usize..].to_vec()
}

fn uint(id: u32, v: u64) -> Vec<u8> {
    let bytes = v.to_be_bytes();
    let skip = bytes.iter().take_while(|&&b| b == 0).count().min(7);
    element(id, &bytes[skip..])
}

fn float(id: u32, v: f64) -> Vec<u8> {
    element(id, &v.to_be_bytes())
}

fn string(id: u32, s: &str) -> Vec<u8> {
    element(id, s.as_bytes())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::clip::{ClipPlayer, PlayOptions};
    use crate::video::{test_pattern, Encoder};
    use matroska_demuxer::{Frame, MatroskaFile, TrackType};
    use std::io::Cursor;
    use std::sync::Arc;

    #[test]
    fn sizes_are_the_shortest_that_hold_them() {
        assert_eq!(size(0), vec![0x80]);
        assert_eq!(size(126), vec![0xFE]);
        // 127 is all ones in one byte, which means "unknown".
        assert_eq!(size(127), vec![0x40, 0x7F]);
        assert_eq!(size(300), vec![0x41, 0x2C]);
    }

    /// A clip made here reads back as the player and the server read clips:
    /// its pictures, its sound, its size and length, and its tag.
    #[test]
    fn a_written_clip_reads_back() {
        let mut enc = Encoder::for_clip();
        let mut sound = crate::opus::Encoder::new(16_000).unwrap();
        let mut w = Writer::new(320, 240, Some(16_000));
        w.tag("CARTOPOLIS_POSE", "cartopolis:pose {}");
        // A second at 10 fps, and 20 ms sound packets beside it. The sound
        // starts before the first picture, as a microphone's often does.
        for n in 0..10u64 {
            let frame = enc.encode(&test_pattern(n), n == 0).unwrap();
            w.video(40 + n * 100, frame);
        }
        for n in 0..55u64 {
            let pcm = crate::video::test_tone(16_000, n);
            w.audio(n * 20, sound.encode(&pcm).unwrap());
        }
        let bytes = w.finish();

        let mut mkv = MatroskaFile::open(Cursor::new(&bytes)).unwrap();
        let kinds: Vec<_> = mkv
            .tracks()
            .iter()
            .map(|t| (t.track_type(), t.codec_id().to_string()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (TrackType::Video, "V_VP8".to_string()),
                (TrackType::Audio, "A_OPUS".to_string())
            ]
        );
        let mut frame = Frame::default();
        let (mut pictures, mut packets, mut first) = (0, 0, None);
        while mkv.next_frame(&mut frame).unwrap() {
            first.get_or_insert(frame.timestamp);
            if frame.track == 1 {
                pictures += 1;
            } else {
                packets += 1;
            }
        }
        assert_eq!(pictures, 10);
        // The sound before the first picture is cut: it starts at 40 ms.
        assert_eq!(packets, 55 - 2);
        assert_eq!(first, Some(0), "the clip starts at its first picture");

        let player = ClipPlayer::start(
            Arc::new(bytes),
            PlayOptions {
                started_at_ms: 0,
                looped: true,
                max_width: 320,
                sound: None,
            },
        )
        .unwrap();
        assert_eq!((player.info.width, player.info.height), (320, 240));
        assert_eq!(player.info.duration_ms, 900);
        assert!(player.info.has_audio);
    }

    #[test]
    fn a_long_clip_is_cut_into_clusters_a_block_offset_can_reach() {
        let mut w = Writer::new(16, 16, None);
        // A keyframe, then only inter frames for a minute.
        w.video(0, vec![0x10, 0, 0]);
        for s in 1..=60 {
            w.video(s * 1000, vec![0x11, 0, 0]);
        }
        let bytes = w.finish();
        let mut mkv = MatroskaFile::open(Cursor::new(&bytes)).unwrap();
        let mut frame = Frame::default();
        let mut last = 0;
        while mkv.next_frame(&mut frame).unwrap() {
            last = frame.timestamp;
        }
        assert_eq!(last, 60_000);
    }
}
