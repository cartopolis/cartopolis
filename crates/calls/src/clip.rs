//! Playing a clip on a prim (`docs/notes/video-on-a-prim.md`): a WebM of VP8
//! video and Opus sound, decoded on a thread of its own at the position a
//! shared start time says it has reached.
//!
//! The same decoders a call uses (`video::Decoder`, `opus::Decoder`); what this
//! adds is the clock. Everybody near the prim plays from `now - started_at`, so
//! they see the same moment without talking to each other. Two consequences:
//!
//! - **Arriving late seeks.** A clip is indexed when it opens (the container
//!   walked once, nothing decoded), so a player that starts three minutes in
//!   decodes from the last keyframe before that point, not from the start —
//!   VP8 cannot skip between keyframes, but it can start at one.
//! - **Falling behind drops pictures, never time.** Frames between the decoded
//!   one and the clock are decoded (they are references) but not shown; sound
//!   older than a moment is dropped rather than played late.

use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use matroska_demuxer::{Frame, MatroskaFile, TrackType};

use crate::opus;
use crate::video::{self, I420};

/// One decoded picture, RGBA, ready to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Where a clip's sound goes: mono samples at the rate the player was given.
pub type SoundRing = Arc<Mutex<VecDeque<f32>>>;

/// How the clip is to be played.
#[derive(Debug, Clone)]
pub struct PlayOptions {
    /// When it started, Unix ms (the prim's `PrimVideo::started_at_ms`).
    pub started_at_ms: u64,
    pub looped: bool,
    /// Pictures wider than this are scaled down before display: a phone need
    /// not upload 720p to a screen that covers a fifth of its display.
    pub max_width: u32,
    /// The sound's rate and ring, or `None` for a silent player.
    pub sound: Option<(u32, SoundRing)>,
}

/// What opening a clip learned about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipInfo {
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
    pub has_audio: bool,
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    picture: Mutex<Option<Picture>>,
}

/// A clip playing. Dropping it stops the thread.
pub struct ClipPlayer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    pub info: ClipInfo,
}

/// Where each frame is, found by walking the container once.
struct Index {
    video_track: u64,
    audio_track: Option<u64>,
    /// Milliseconds per container tick unit (timestamp scale / 1e6).
    ns_per_tick: u64,
    /// Video keyframes, ms.
    keyframes: Vec<u64>,
    /// The last frame of any track, ms. The clip's length for playback: a
    /// sound track routinely runs a little past the last picture, and a clock
    /// that wraps before reaching it would wait on that packet for ever.
    end_ms: u64,
    info: ClipInfo,
}

fn index(bytes: &[u8]) -> Result<Index, String> {
    let mut mkv =
        MatroskaFile::open(Cursor::new(bytes)).map_err(|e| format!("not a WebM file ({e})"))?;
    let mut video_track = None;
    let mut audio_track = None;
    let mut size = (0, 0);
    for t in mkv.tracks() {
        match t.track_type() {
            TrackType::Video if t.codec_id() == "V_VP8" => {
                video_track = Some(t.track_number().get());
                if let Some(v) = t.video() {
                    size = (v.pixel_width().get() as u32, v.pixel_height().get() as u32);
                }
            }
            TrackType::Audio if t.codec_id() == "A_OPUS" => {
                audio_track = Some(t.track_number().get());
            }
            _ => {}
        }
    }
    let video_track = video_track.ok_or("no VP8 video track")?;
    let ns_per_tick = mkv.info().timestamp_scale().get();
    let mut keyframes = Vec::new();
    let mut last = 0;
    let mut end_ms = 0;
    let mut frame = Frame::default();
    while mkv.next_frame(&mut frame).map_err(|e| e.to_string())? {
        let ms = frame.timestamp * ns_per_tick / 1_000_000;
        end_ms = end_ms.max(ms);
        if frame.track == video_track {
            last = last.max(ms);
            if frame.data.first().is_some_and(|b| b & 1 == 0) {
                keyframes.push(ms);
            }
        }
    }
    if keyframes.is_empty() {
        return Err("the clip has no keyframe".into());
    }
    Ok(Index {
        video_track,
        audio_track,
        ns_per_tick,
        keyframes,
        end_ms,
        info: ClipInfo {
            width: size.0,
            height: size.1,
            duration_ms: last,
            has_audio: audio_track.is_some(),
        },
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl ClipPlayer {
    /// Open a clip and start playing it on a thread.
    pub fn start(bytes: Arc<Vec<u8>>, options: PlayOptions) -> Result<Self, String> {
        Self::start_with_clock(bytes, options, now_ms)
    }

    /// [`ClipPlayer::start`] with a clock of the caller's, for tests.
    pub fn start_with_clock(
        bytes: Arc<Vec<u8>>,
        options: PlayOptions,
        clock: fn() -> u64,
    ) -> Result<Self, String> {
        let idx = index(&bytes)?;
        let info = idx.info.clone();
        let shared = Arc::new(Shared::default());
        let s = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("clip".into())
            .spawn(move || play(&bytes, &idx, &options, clock, &s))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            thread: Some(thread),
            info,
        })
    }

    /// The newest picture since last asked.
    #[must_use]
    pub fn take_picture(&self) -> Option<Picture> {
        self.shared.picture.lock().ok()?.take()
    }
}

impl Drop for ClipPlayer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Where in the clip `now` is: `None` before it starts, and past the end of a
/// clip that does not loop, the end.
fn position(now: u64, options: &PlayOptions, duration_ms: u64) -> Option<u64> {
    let elapsed = now.checked_sub(options.started_at_ms)?;
    // One frame's worth past the last timestamp is the clip's length.
    let length = duration_ms + 40;
    Some(if options.looped {
        elapsed % length
    } else {
        elapsed.min(duration_ms)
    })
}

fn show(frame: &I420, max_width: u32) -> Picture {
    let frame = if frame.width > max_width {
        let h = (u64::from(frame.height) * u64::from(max_width) / u64::from(frame.width)) as u32;
        frame.scaled(max_width, h.max(2) & !1)
    } else {
        frame.clone()
    };
    Picture {
        width: frame.width,
        height: frame.height,
        rgba: frame.to_rgba(),
    }
}

fn play(bytes: &[u8], idx: &Index, options: &PlayOptions, clock: fn() -> u64, s: &Shared) {
    let ticks_to_ms = |t: u64| t * idx.ns_per_tick / 1_000_000;
    // Wait for the start.
    let pos = loop {
        if s.stop.load(Ordering::Relaxed) {
            return;
        }
        if let Some(p) = position(clock(), options, idx.end_ms) {
            break p;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut from = pos;
    'pass: loop {
        // Start at the last keyframe at or before `from`.
        let key = idx
            .keyframes
            .iter()
            .rev()
            .find(|k| **k <= from)
            .copied()
            .unwrap_or(0);
        let Ok(mut mkv) = MatroskaFile::open(Cursor::new(bytes)) else {
            return;
        };
        let mut pictures = video::Decoder::new();
        let mut sound = idx
            .audio_track
            .and(options.sound.as_ref())
            .and_then(|_| opus::Decoder::new(48_000).ok());
        let mut frame = Frame::default();
        let mut last_ms = 0;
        loop {
            if s.stop.load(Ordering::Relaxed) {
                return;
            }
            match mkv.next_frame(&mut frame) {
                Ok(true) => {}
                // The end, or a damaged tail: a looping clip goes round again,
                // any other holds its last picture.
                _ => {
                    if options.looped {
                        from = 0;
                        // Wait for the clock to come round to the start.
                        while position(clock(), options, idx.end_ms)
                            .is_some_and(|p| p > last_ms / 2)
                        {
                            if s.stop.load(Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        continue 'pass;
                    }
                    return;
                }
            }
            let ms = ticks_to_ms(frame.timestamp);
            last_ms = last_ms.max(ms);
            if ms < key {
                continue;
            }
            // Wait until the clock reaches this frame; notice a jump back (the
            // loop coming round, or a clock set back) and start over.
            loop {
                let Some(now) = position(clock(), options, idx.end_ms) else {
                    continue 'pass;
                };
                if now + 1_000 < ms && options.looped {
                    from = now;
                    continue 'pass;
                }
                if now + 5 >= ms {
                    break;
                }
                if s.stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis((ms - now).min(10)));
            }
            let now = position(clock(), options, idx.end_ms).unwrap_or(ms);
            if frame.track == idx.video_track {
                let Ok(decoded) = pictures.decode(&frame.data) else {
                    continue;
                };
                // Only a picture that is still current is worth converting and
                // uploading; the rest were decoded as references.
                if now < ms + 100 {
                    if let Ok(mut slot) = s.picture.lock() {
                        *slot = Some(show(&decoded, options.max_width));
                    }
                }
            } else if Some(frame.track) == idx.audio_track {
                let (Some(dec), Some((rate, ring))) = (sound.as_mut(), options.sound.as_ref())
                else {
                    continue;
                };
                // Decode every packet (Opus carries state), play only fresh ones.
                let Ok(pcm) = dec.decode(&frame.data) else {
                    continue;
                };
                if now > ms + 200 {
                    continue;
                }
                let step = (48_000 / rate).max(1) as usize;
                if let Ok(mut r) = ring.lock() {
                    r.extend(
                        pcm.chunks(step)
                            .map(|c| c.iter().sum::<f32>() / c.len() as f32),
                    );
                    let cap = *rate as usize / 2;
                    while r.len() > cap {
                        r.pop_front();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of 64×48 VP8 at 5 fps with Opus sound, made by ffmpeg.
    const TINY: &[u8] = include_bytes!("../../simulator/tests/integration/fixtures/tiny.webm");

    #[test]
    fn a_clip_is_indexed_without_decoding_it() {
        let idx = index(TINY).unwrap();
        assert_eq!((idx.info.width, idx.info.height), (64, 48));
        assert!(idx.info.has_audio);
        assert!(
            (700..=1000).contains(&idx.info.duration_ms),
            "the last picture: {}",
            idx.info.duration_ms
        );
        assert!(idx.end_ms >= idx.info.duration_ms, "the sound runs on");
        assert_eq!(idx.keyframes.first(), Some(&0));
        assert!(index(b"not a clip").is_err());
    }

    #[test]
    fn position_follows_the_shared_clock() {
        let o = PlayOptions {
            started_at_ms: 10_000,
            looped: true,
            max_width: 640,
            sound: None,
        };
        assert_eq!(position(9_000, &o, 960), None, "not started");
        assert_eq!(position(10_500, &o, 960), Some(500));
        assert_eq!(position(11_500, &o, 960), Some(500), "looped: 1000 ms long");
        let once = PlayOptions { looped: false, ..o };
        assert_eq!(position(20_000, &once, 960), Some(960), "held at the end");
    }

    #[test]
    fn a_player_shows_pictures_and_plays_sound_from_the_shared_start() {
        let ring: SoundRing = Arc::default();
        let player = ClipPlayer::start(
            Arc::new(TINY.to_vec()),
            PlayOptions {
                // Started half a second ago: joining mid-clip.
                started_at_ms: now_ms() - 500,
                looped: true,
                max_width: 32,
                sound: Some((16_000, ring.clone())),
            },
        )
        .unwrap();
        let mut pictures = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Some(p) = player.take_picture() {
                assert_eq!((p.width, p.height), (32, 24), "scaled to max_width");
                assert_eq!(p.rgba.len(), 32 * 24 * 4);
                pictures += 1;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // 5 fps for ~3 s, going round the one-second clip.
        assert!(pictures >= 8, "{pictures} pictures");
        assert!(!ring.lock().unwrap().is_empty(), "the sound played");
    }
}
