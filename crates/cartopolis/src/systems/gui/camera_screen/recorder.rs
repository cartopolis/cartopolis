//! The camera's **Video**: a clip of what the screen shows in any of the three
//! modes, as a WebM of VP8 pictures and Opus sound (`cartopolis_calls::webm`).
//!
//! Pictures go to an encoder on a thread of their own, one waiting at most: a
//! picture that finds the queue full is dropped, and the clip's timestamps
//! keep the time, so a slow encoder makes a choppier clip rather than a slower
//! one. Each picture is put together on that thread too — the photograph, the
//! world cut from a window capture, or both side by side — so the frame loop
//! only hands pixels over.
//!
//! The encoder is the phone's VP8 (`platform::hw_video::clip_encoder`) where
//! it opens, else the pure-Rust one at a smaller size, which is what a desktop
//! records with. Sound is the microphone, resampled to 16 kHz and Opus-coded on
//! the app's thread: a 20 ms frame is well under a millisecond.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use cartopolis_calls::video::I420;
use cartopolis_calls::webm::Writer;
use cartopolis_core::video_limits::{MAX_VIDEO_BYTES, MAX_VIDEO_MS};

use crate::platform::audio::AudioInput;
use crate::systems::voice;

/// Pictures a second a clip is recorded at: the camera's own delivery rate.
pub const FPS: u32 = cartopolis_calls::video::FPS;

/// A clip's largest picture with the phone's encoder, and with the pure-Rust
/// one (which takes ~45 ms for 320×240 and so records a desktop's clip small).
const HW_PIXELS: u32 = 1280 * 720;
const SW_PIXELS: u32 = 480 * 360;
/// No side longer than this: what a prim plays.
const MAX_SIDE: u32 = 1280;

/// Stop this far short of the store's limit, for the frames still in flight.
const BYTES_MARGIN: usize = 2 * 1024 * 1024;

/// The window as it was drawn, for the world in a clip: its pixels as the
/// capture handed them over, and which way round their bytes are.
pub struct Grab {
    pub pixels: Vec<u8>,
    pub size: [u32; 2],
    pub bgra: bool,
}

impl Grab {
    /// A capture's pixels, converted only when they are in neither 8-bit
    /// order (an HDR window).
    #[must_use]
    pub fn from_image(image: &bevy::image::Image) -> Option<Self> {
        use bevy::render::render_resource::TextureFormat as F;
        let size = [image.width(), image.height()];
        match image.texture_descriptor.format {
            F::Bgra8Unorm | F::Bgra8UnormSrgb => Some(Self {
                pixels: image.data.clone()?,
                size,
                bgra: true,
            }),
            F::Rgba8Unorm | F::Rgba8UnormSrgb => Some(Self {
                pixels: image.data.clone()?,
                size,
                bgra: false,
            }),
            _ => {
                let rgba = image.clone().try_into_dynamic().ok()?.to_rgba8();
                Some(Self {
                    pixels: rgba.into_raw(),
                    size,
                    bgra: false,
                })
            }
        }
    }

    /// The part inside `rect` (fractions of the window) as a picture, with
    /// even sides.
    fn cut(&self, rect: [f32; 4]) -> Option<I420> {
        let [w, h] = self.size;
        let x0 = ((rect[0].clamp(0.0, 1.0) * w as f32) as u32) & !1;
        let y0 = ((rect[1].clamp(0.0, 1.0) * h as f32) as u32) & !1;
        let x1 = ((rect[2].clamp(0.0, 1.0) * w as f32) as u32).min(w);
        let y1 = ((rect[3].clamp(0.0, 1.0) * h as f32) as u32).min(h);
        let (cw, ch) = (x1.saturating_sub(x0) & !1, y1.saturating_sub(y0) & !1);
        if cw < 2 || ch < 2 {
            return None;
        }
        let stride = w as usize * 4;
        let at = y0 as usize * stride + x0 as usize * 4;
        I420::from_rgba(cw, ch, stride, self.pixels.get(at..)?, self.bgra)
    }
}

/// What one picture of a clip is made of.
pub enum Source {
    /// The photograph alone (Real).
    Photo(I420),
    /// The world alone, cut from the window (In world).
    World { grab: Grab, rect: [f32; 4] },
    /// The photograph beside the world (Mixed).
    Mixed {
        photo: I420,
        grab: Grab,
        rect: [f32; 4],
    },
}

/// How a clip's picture is laid out: its size, and in Mixed the size of each
/// half and whether they sit side by side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub size: (u32, u32),
    half: (u32, u32),
    side_by_side: bool,
}

impl Layout {
    /// A clip of one picture of shape `aspect`, or (`mixed`) two of it side
    /// by side when it is wider than tall and one above the other when not,
    /// in at most `pixels`. Every side a multiple of 16, which every encoder
    /// takes.
    #[must_use]
    pub fn new(aspect: f32, mixed: bool, pixels: u32) -> Self {
        let aspect = aspect.clamp(0.2, 5.0);
        let side_by_side = aspect >= 1.0;
        let whole = match (mixed, side_by_side) {
            (false, _) => aspect,
            (true, true) => aspect * 2.0,
            (true, false) => aspect / 2.0,
        };
        let mut w = (pixels as f32 * whole).sqrt();
        let mut h = w / whole;
        let long = w.max(h);
        if long > MAX_SIDE as f32 {
            let s = MAX_SIDE as f32 / long;
            w *= s;
            h *= s;
        }
        let parts = if mixed { 2.0 } else { 1.0 };
        let sixteen = |v: f32| ((v as u32) / 16 * 16).max(16);
        let half = if !mixed {
            (sixteen(w), sixteen(h))
        } else if side_by_side {
            (sixteen(w / parts), sixteen(h))
        } else {
            (sixteen(w), sixteen(h / parts))
        };
        let size = match (mixed, side_by_side) {
            (false, _) => half,
            (true, true) => (half.0 * 2, half.1),
            (true, false) => (half.0, half.1 * 2),
        };
        Self {
            size,
            half,
            side_by_side,
        }
    }

    /// The clip's picture from what was on screen.
    fn compose(&self, source: Source) -> Option<I420> {
        let (hw, hh) = self.half;
        match source {
            Source::Photo(p) => Some(shrink(&p, hw, hh)),
            Source::World { grab, rect } => grab.cut(rect).map(|w| shrink(&w, hw, hh)),
            Source::Mixed { photo, grab, rect } => {
                let photo = shrink(&photo, hw, hh);
                let world = grab
                    .cut(rect)
                    .map_or_else(|| I420::black(hw, hh), |w| shrink(&w, hw, hh));
                Some(stitch(&photo, &world, self.side_by_side))
            }
        }
    }
}

/// `p` at `w`×`h`, each sample the mean of the ones it covers: nearest
/// neighbour (`I420::scaled`) shimmers in a moving picture shrunk threefold.
#[must_use]
pub fn shrink(p: &I420, w: u32, h: u32) -> I420 {
    if (p.width, p.height) == (w, h) {
        return p.clone();
    }
    if p.width < w || p.height < h {
        return p.scaled(w, h);
    }
    let plane = |src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize| -> Vec<u8> {
        let mut out = vec![0u8; dw * dh];
        for y in 0..dh {
            let (y0, y1) = (y * sh / dh, ((y + 1) * sh / dh).max(y * sh / dh + 1));
            for x in 0..dw {
                let (x0, x1) = (x * sw / dw, ((x + 1) * sw / dw).max(x * sw / dw + 1));
                let mut sum = 0u32;
                for row in y0..y1 {
                    sum += src[row * sw + x0..row * sw + x1]
                        .iter()
                        .map(|&v| u32::from(v))
                        .sum::<u32>();
                }
                out[y * dw + x] = (sum / ((y1 - y0) * (x1 - x0)) as u32) as u8;
            }
        }
        out
    };
    let (sw, sh) = (p.width as usize, p.height as usize);
    let (dw, dh) = (w as usize, h as usize);
    let (scw, sch) = (sw.div_ceil(2), sh.div_ceil(2));
    let (dcw, dch) = (dw.div_ceil(2), dh.div_ceil(2));
    I420 {
        width: w,
        height: h,
        y: plane(&p.y, sw, sh, dw, dh),
        u: plane(&p.u, scw, sch, dcw, dch),
        v: plane(&p.v, scw, sch, dcw, dch),
    }
}

/// Two pictures of one even size, side by side or `a` above `b`.
#[must_use]
pub fn stitch(a: &I420, b: &I420, side_by_side: bool) -> I420 {
    let (w, h) = (a.width as usize, a.height as usize);
    if !side_by_side {
        return I420 {
            width: a.width,
            height: a.height * 2,
            y: [a.y.as_slice(), b.y.as_slice()].concat(),
            u: [a.u.as_slice(), b.u.as_slice()].concat(),
            v: [a.v.as_slice(), b.v.as_slice()].concat(),
        };
    }
    let rows = |pa: &[u8], pb: &[u8], rw: usize, rh: usize| -> Vec<u8> {
        let mut out = Vec::with_capacity(rw * rh * 2);
        for r in 0..rh {
            out.extend_from_slice(&pa[r * rw..(r + 1) * rw]);
            out.extend_from_slice(&pb[r * rw..(r + 1) * rw]);
        }
        out
    };
    let (cw, ch) = (w / 2, h / 2);
    I420 {
        width: a.width * 2,
        height: a.height,
        y: rows(&a.y, &b.y, w, h),
        u: rows(&a.u, &b.u, cw, ch),
        v: rows(&a.v, &b.v, cw, ch),
    }
}

/// The encoder a clip uses.
enum ClipEncoder {
    #[cfg(target_os = "android")]
    Phone(crate::platform::hw_video::ClipEncoder),
    Rust(cartopolis_calls::video::Encoder),
}

impl ClipEncoder {
    fn encode(&mut self, frame: &I420, ms: u64) -> Result<Vec<(u64, Vec<u8>)>, String> {
        match self {
            #[cfg(target_os = "android")]
            Self::Phone(e) => e.encode(frame, ms),
            Self::Rust(e) => e
                .encode(frame, false)
                .map(|f| vec![(ms, f)])
                .map_err(|e| e.to_string()),
        }
    }

    fn finish(&mut self) -> Vec<(u64, Vec<u8>)> {
        match self {
            #[cfg(target_os = "android")]
            Self::Phone(e) => e.finish(),
            Self::Rust(_) => Vec::new(),
        }
    }
}

/// The phone's encoder for `aspect` (and Mixed), and the layout it allows;
/// else the pure-Rust one's.
fn open_encoder(aspect: f32, mixed: bool) -> (ClipEncoder, Layout) {
    #[cfg(target_os = "android")]
    {
        let layout = Layout::new(aspect, mixed, HW_PIXELS);
        if let Some(e) = crate::platform::hw_video::clip_encoder(layout.size.0, layout.size.1) {
            return (ClipEncoder::Phone(e), layout);
        }
    }
    let _ = HW_PIXELS;
    (
        ClipEncoder::Rust(cartopolis_calls::video::Encoder::for_clip()),
        Layout::new(aspect, mixed, SW_PIXELS),
    )
}

/// A clip being recorded.
pub struct Recorder {
    started: Instant,
    frames: Option<SyncSender<(Source, u64)>>,
    writer: Arc<Mutex<Writer>>,
    /// The encoder's thread; its answer is the finished file.
    thread: Option<JoinHandle<Result<Vec<u8>, String>>>,
    /// Pictures handed over, and dropped because the encoder was busy.
    pub sent: u32,
    pub dropped: u32,
    /// The next picture is due at this many ms.
    next_ms: u64,
    mic: Option<AudioInput>,
    resampler: voice::Resampler,
    capture: Vec<i16>,
    pending: Vec<i16>,
    opus: Option<cartopolis_calls::opus::Encoder>,
    /// When the sound's first sample was heard, ms into the clip, and how
    /// many 20 ms packets have been written since.
    sound_from_ms: Option<u64>,
    packets: u64,
    /// Why the clip has no sound, when it has none.
    pub sound_error: Option<String>,
}

/// Why a recording stopped by itself.
pub const FULL: &str = "The clip reached its longest; it was stopped there.";

impl Recorder {
    /// Start recording pictures of shape `aspect` (one, or two side by side
    /// in Mixed), with the clip's tags.
    pub fn start(aspect: f32, mixed: bool, tags: Vec<(String, String)>) -> Result<Self, String> {
        let sound = AudioInput::available();
        let (tx, rx) = std::sync::mpsc::sync_channel::<(Source, u64)>(1);
        // The encoder is opened on its thread (a phone's codec can take a
        // moment) and decides the picture's size, which it gives the writer
        // then; sound heard meanwhile is kept.
        let mut w = Writer::new(16, 16, sound.then_some(voice::VOICE_RATE));
        for (n, v) in &tags {
            w.tag(n, v);
        }
        let writer = Arc::new(Mutex::new(w));
        let w = Arc::clone(&writer);
        let thread = std::thread::Builder::new()
            .name("camera-video".into())
            .spawn(move || encode(rx, aspect, mixed, &w))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            started: Instant::now(),
            frames: Some(tx),
            writer,
            thread: Some(thread),
            sent: 0,
            dropped: 0,
            next_ms: 0,
            mic: sound.then(AudioInput::start),
            resampler: voice::Resampler::default(),
            capture: Vec::new(),
            pending: Vec::new(),
            opus: cartopolis_calls::opus::Encoder::new(voice::VOICE_RATE).ok(),
            sound_from_ms: None,
            packets: 0,
            sound_error: None,
        })
    }

    /// Milliseconds since the recording began.
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Whether the next picture is due.
    #[must_use]
    pub fn due(&self) -> bool {
        self.frames.is_some() && self.elapsed_ms() >= self.next_ms
    }

    /// Hand over a picture of the screen as it was at `ms`.
    pub fn picture(&mut self, source: Source, ms: u64) {
        let Some(tx) = self.frames.as_ref() else {
            return;
        };
        self.next_ms = ms + 1000 / u64::from(FPS);
        match tx.try_send((source, ms)) {
            Ok(()) => self.sent += 1,
            Err(TrySendError::Full(_)) => self.dropped += 1,
            Err(TrySendError::Disconnected(_)) => self.frames = None,
        }
    }

    /// Take the microphone's new sound into the clip, and stop the clip at
    /// its limits. Returns why it stopped, when it has.
    pub fn tick(&mut self) -> Option<&'static str> {
        self.hear();
        let full = self.writer.lock().map_or(0, |w| w.len());
        (self.frames.is_some()
            && (full + BYTES_MARGIN >= MAX_VIDEO_BYTES || self.elapsed_ms() >= MAX_VIDEO_MS))
            .then_some(FULL)
    }

    fn hear(&mut self) {
        let Some(mic) = self.mic.as_ref() else {
            return;
        };
        if let Some(e) = mic.error() {
            self.sound_error = Some(e);
            self.mic = None;
            return;
        }
        let Some(rate) = mic.sample_rate() else {
            return;
        };
        self.capture.clear();
        mic.drain(&mut self.capture, usize::MAX);
        if self.capture.is_empty() {
            return;
        }
        // The first sound is placed at when it was heard: now, less its own
        // length.
        if self.sound_from_ms.is_none() {
            let heard = self.capture.len() as u64 * 1000 / u64::from(rate);
            self.sound_from_ms = Some(self.elapsed_ms().saturating_sub(heard));
        }
        self.resampler.push(&self.capture, rate, &mut self.pending);
        let frame = voice::FRAME_SAMPLES;
        let Some(opus) = self.opus.as_mut() else {
            return;
        };
        let from = self.sound_from_ms.unwrap_or(0);
        while self.pending.len() >= frame {
            let pcm: Vec<f32> = self
                .pending
                .drain(..frame)
                .map(|s| f32::from(s) / f32::from(i16::MAX))
                .collect();
            if let Ok(packet) = opus.encode(&pcm) {
                if let Ok(mut w) = self.writer.lock() {
                    w.audio(from + self.packets * 20, packet);
                }
            }
            self.packets += 1;
        }
    }

    /// Stop taking pictures and sound; the encoder finishes what it has and
    /// writes the file ([`Recorder::finished`]).
    pub fn stop(&mut self) {
        self.hear();
        self.mic = None;
        self.frames = None;
    }

    /// Whether [`Recorder::stop`] has been called.
    #[must_use]
    pub fn stopping(&self) -> bool {
        self.frames.is_none()
    }

    /// The file, once the encoder has written it after a stop.
    pub fn finished(&mut self) -> Option<Result<Vec<u8>, String>> {
        if !self.stopping() || !self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return None;
        }
        let thread = self.thread.take()?;
        Some(
            thread
                .join()
                .unwrap_or_else(|_| Err("the recording stopped unexpectedly".into())),
        )
    }
}

/// The encoder's thread: each picture put together and encoded as it comes,
/// then the file.
fn encode(
    rx: Receiver<(Source, u64)>,
    aspect: f32,
    mixed: bool,
    writer: &Mutex<Writer>,
) -> Result<Vec<u8>, String> {
    let (mut enc, layout) = open_encoder(aspect, mixed);
    writer
        .lock()
        .map_err(|e| e.to_string())?
        .set_size(layout.size.0, layout.size.1);
    let started = Instant::now();
    let mut pictures = 0u32;
    while let Ok((source, ms)) = rx.recv() {
        let Some(picture) = layout.compose(source) else {
            continue;
        };
        let out = match enc.encode(&picture, ms) {
            Ok(out) => out,
            Err(e) => {
                // The phone's codec gave up: the rest of the clip in software,
                // from a keyframe, at the size already chosen.
                tracing::warn!(%e, "camera video: the encoder failed; encoding in software");
                enc = ClipEncoder::Rust(cartopolis_calls::video::Encoder::for_clip());
                enc.encode(&picture, ms)?
            }
        };
        pictures += 1;
        if let Ok(mut w) = writer.lock() {
            for (at, frame) in out {
                w.video(at, frame);
            }
        }
    }
    let rest = enc.finish();
    let mut w = writer.lock().map_err(|e| e.to_string())?;
    for (at, frame) in rest {
        w.video(at, frame);
    }
    let w = std::mem::replace(&mut *w, Writer::new(16, 16, None));
    if w.is_empty() {
        return Err("No picture was recorded.".into());
    }
    tracing::info!(
        pictures,
        seconds = started.elapsed().as_secs_f32(),
        size = ?layout.size,
        bytes = w.len(),
        "camera video: recorded"
    );
    Ok(w.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: u32, h: u32, y: u8) -> I420 {
        let mut p = I420::black(w, h);
        p.y.iter_mut().for_each(|v| *v = y);
        p
    }

    /// Held sideways the halves sit side by side, upright one above the
    /// other; every side is a multiple of 16 and the clip stays within what a
    /// prim plays.
    #[test]
    fn a_mixed_clip_has_two_halves_of_the_photographs_shape() {
        let wide = Layout::new(4.0 / 3.0, true, HW_PIXELS);
        assert!(wide.side_by_side);
        assert_eq!(wide.size, (wide.half.0 * 2, wide.half.1));
        let tall = Layout::new(3.0 / 4.0, true, HW_PIXELS);
        assert!(!tall.side_by_side);
        assert_eq!(tall.size, (tall.half.0, tall.half.1 * 2));
        for l in [wide, tall, Layout::new(4.0 / 3.0, false, HW_PIXELS)] {
            let (w, h) = l.size;
            assert_eq!((w % 16, h % 16), (0, 0), "{l:?}");
            assert!(w.max(h) <= MAX_SIDE && w * h <= HW_PIXELS, "{l:?}");
            let half = l.half.0 as f32 / l.half.1 as f32;
            assert!(
                (half - 4.0 / 3.0).abs() < 0.1 || (half - 0.75).abs() < 0.1,
                "{l:?}"
            );
        }
    }

    #[test]
    fn halves_are_stitched_photograph_first() {
        let (a, b) = (flat(4, 2, 200), flat(4, 2, 50));
        let side = stitch(&a, &b, true);
        assert_eq!((side.width, side.height), (8, 2));
        assert_eq!(&side.y[..8], &[200, 200, 200, 200, 50, 50, 50, 50]);
        assert_eq!(side.u.len(), 4);
        let stacked = stitch(&a, &b, false);
        assert_eq!((stacked.width, stacked.height), (4, 4));
        assert_eq!((stacked.y[0], stacked.y[15]), (200, 50));
    }

    #[test]
    fn a_shrunk_picture_is_the_mean_of_what_it_covers() {
        let mut p = flat(4, 4, 0);
        // A checkerboard of 0 and 200 averages to 100, where nearest
        // neighbour would pick one of the two.
        for (i, v) in p.y.iter_mut().enumerate() {
            *v = if (i % 4 + i / 4) % 2 == 0 { 0 } else { 200 };
        }
        let s = shrink(&p, 2, 2);
        assert_eq!(s.y, vec![100; 4]);
        assert_eq!(s.u.len(), 1);
    }

    /// The world is cut from a window capture in either byte order.
    #[test]
    fn the_world_is_cut_from_the_capture() {
        let (w, h) = (8u32, 4u32);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        // The right half white.
        for y in 0..h {
            for x in 4..w {
                let o = ((y * w + x) * 4) as usize;
                pixels[o..o + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let grab = Grab {
            pixels,
            size: [w, h],
            bgra: true,
        };
        let cut = grab.cut([0.5, 0.0, 1.0, 1.0]).unwrap();
        assert_eq!((cut.width, cut.height), (4, 4));
        assert!(cut.y.iter().all(|&v| v > 200), "{:?}", cut.y);
    }
}
