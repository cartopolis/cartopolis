//! A call's video encoded by the phone's own codecs (`docs/notes/calls.md`,
//! "Hardware encoding on the phone"): `MediaCodec` through its C API
//! (`libmediandk`), so like the camera it needs no Java.
//!
//! **Which encoder.** The first that opens, in this order:
//!
//! 1. the system's preferred VP9 encoder, if it is a hardware one;
//! 2. the system's preferred VP8 encoder, if it is a hardware one;
//! 3. Google's software VP9 encoder that every Android build carries
//!    (`c2.android.vp9.encoder`, which is libvpx — C, but the system's, not
//!    compiled by us);
//! 4. none, and the call encodes in pure Rust as on every other platform.
//!
//! A hardware VP9 *encoder* is rare (Qualcomm has historically shipped VP9 as
//! a decoder only), so 2 is the likely answer on a Snapdragon. Receivers decode
//! VP8 and VP9 alike, so the choice is the sender's alone. Whatever is chosen
//! runs behind `video::WithFallback`: its first error hands the stream to the
//! Rust encoder from a keyframe, for the rest of the call.
//!
//! **Not run on a device.** The emulator on the VPS cannot run this app, so the
//! codec choice, the input layout and the bitstream have only been compiled.
//! Two log lines say what happened on a phone: `call video: encoder chosen` at
//! the start and `call video: encoding` every few seconds with the mean time a
//! picture took. `CARTO_VIDEO_ENCODER=software` skips all of this, for an A/B.

use std::ffi::{c_char, CStr, CString};
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use cartopolis_calls::media::Codec;
use cartopolis_calls::session::EncoderFactory;
use cartopolis_calls::video::{self, Error, VideoEncode, WithFallback, I420};
use ndk_sys as sys;

/// `MediaCodecInfo.CodecCapabilities`' colour formats: NV12 (what nearly every
/// hardware encoder takes) and I420.
const COLOR_YUV420_SEMIPLANAR: i32 = 21;
const COLOR_YUV420_PLANAR: i32 = 19;
const CONFIGURE_FLAG_ENCODE: u32 = 1;
const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;
const INFO_TRY_AGAIN_LATER: isize = -1;

/// Bitrates at the software encoder's 320×240, 15 fps: about what the software
/// encoders measured at the same picture quality (`docs/notes/calls.md`). A
/// hardware encoder rate-controls; it has no constant-quality knob to match
/// the software one by.
const VP9_BITRATE: f64 = 250_000.0;
const VP8_BITRATE: f64 = 450_000.0;

/// The bitrate for `kind` at `width`×`height`: grown with the pixel count to
/// the power 0.6, not linearly — a codec spends less per pixel on a bigger
/// picture of the same scene, and this goes up a phone's uplink through a
/// forwarder that does no congestion control yet. 640×480 (four times the
/// pixels) is 2.3×: ~575 kbit/s for VP9, ~1 Mbit/s for VP8.
fn bitrate(kind: Codec, width: u32, height: u32) -> i32 {
    let base = if kind == Codec::Vp9 {
        VP9_BITRATE
    } else {
        VP8_BITRATE
    };
    let ratio = f64::from(width * height) / f64::from(video::WIDTH * video::HEIGHT);
    (base * ratio.powf(0.6)) as i32
}

/// How long one picture may wait for a free input buffer, and for its frame.
const INPUT_WAIT: Duration = Duration::from_millis(20);
const OUTPUT_WAIT: Duration = Duration::from_millis(40);

/// The encoder for a call's video on this phone. Called on the encoder thread.
#[must_use]
pub fn encoder_factory() -> EncoderFactory {
    Box::new(|| {
        let forced_software =
            std::env::var("CARTO_VIDEO_ENCODER").is_ok_and(|v| v.eq_ignore_ascii_case("software"));
        let primary = if forced_software { None } else { choose() };
        match &primary {
            Some(e) => {
                tracing::info!(name = %e.name, codec = ?e.kind, hardware = e.hardware, "call video: encoder chosen")
            }
            None => tracing::info!("call video: encoder chosen: software (rusty_vp9)"),
        }
        Box::new(WithFallback::new(
            primary.map(|e| Box::new(e) as Box<dyn VideoEncode>),
        ))
    })
}

/// Hardware sends [`video::HW_WIDTH`]×[`video::HW_HEIGHT`] — the camera's own
/// size, a few ms of the encoder's time — and a software codec the call's
/// usual 320×240. `CARTO_VIDEO_SIZE=small` keeps hardware at 320×240 too, for
/// an A/B or a poor uplink.
fn choose() -> Option<MediaEncoder> {
    let small = (video::WIDTH, video::HEIGHT);
    let large = if std::env::var("CARTO_VIDEO_SIZE").is_ok_and(|v| v.eq_ignore_ascii_case("small"))
    {
        small
    } else {
        (video::HW_WIDTH, video::HW_HEIGHT)
    };
    for kind in [Codec::Vp9, Codec::Vp8] {
        match MediaEncoder::open(kind, None, large, large.0, large.1) {
            Ok(e) if e.hardware => return Some(e),
            Ok(e) => tracing::info!(name = %e.name, "call video: not hardware, passed over"),
            Err(why) => tracing::info!(?kind, %why, "call video: no system encoder"),
        }
    }
    MediaEncoder::open(
        Codec::Vp9,
        Some("c2.android.vp9.encoder"),
        small,
        small.0,
        small.1,
    )
    .map_err(|why| tracing::info!(%why, "call video: no system software VP9"))
    .ok()
}

/// Google's software codecs carry these names; everything else a phone lists
/// is the chip maker's, i.e. hardware.
fn is_software(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("c2.android.") || n.starts_with("omx.google.") || n.contains(".sw.")
}

fn mime(kind: Codec) -> &'static CStr {
    match kind {
        Codec::Vp9 => c"video/x-vnd.on2.vp9",
        _ => c"video/x-vnd.on2.vp8",
    }
}

/// One `AMediaCodec` encoder, configured for one picture size.
struct MediaEncoder {
    codec: *mut sys::AMediaCodec,
    name: String,
    /// What the codec is asked for again at a new size.
    by_name: Option<String>,
    kind: Codec,
    hardware: bool,
    /// The size this encoder sends, landscape ([`VideoEncode::picture_size`]).
    base: (u32, u32),
    width: u32,
    height: u32,
    /// How the codec wants its input laid out, from its input format.
    color: i32,
    stride: usize,
    slice_height: usize,
    pictures: u64,
    /// Encode time since the last report, and how many pictures.
    spent: Duration,
    spent_n: u32,
    reported: Instant,
}

// SAFETY: an `AMediaCodec` may be used from any thread, one at a time; this
// one lives on the call's encoder thread and is only ever moved there.
unsafe impl Send for MediaEncoder {}

impl Drop for MediaEncoder {
    fn drop(&mut self) {
        // SAFETY: `codec` came from a create call and is freed only here.
        unsafe {
            sys::AMediaCodec_stop(self.codec);
            sys::AMediaCodec_delete(self.codec);
        }
    }
}

fn ok(status: sys::media_status_t) -> bool {
    status == sys::media_status_t::AMEDIA_OK
}

/// An `AMediaFormat`, deleted on drop.
struct Format(*mut sys::AMediaFormat);

impl Format {
    fn new() -> Self {
        // SAFETY: plain allocation.
        Self(unsafe { sys::AMediaFormat_new() })
    }
    fn int(&self, key: &CStr, value: i32) {
        // SAFETY: a live format and NUL-terminated key.
        unsafe { sys::AMediaFormat_setInt32(self.0, key.as_ptr(), value) }
    }
    fn get_int(&self, key: &CStr) -> Option<i32> {
        let mut v = 0;
        // SAFETY: as above; `v` outlives the call.
        unsafe { sys::AMediaFormat_getInt32(self.0, key.as_ptr(), &mut v) }.then_some(v)
    }
}

impl Drop for Format {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: created by `AMediaFormat_new` or handed over by the
            // codec (`getInputFormat`), whose result the caller owns.
            unsafe {
                sys::AMediaFormat_delete(self.0);
            }
        }
    }
}

impl MediaEncoder {
    /// Open `kind` (or the codec named `by_name`) to send `base` (landscape),
    /// configured now for a `width`×`height` picture — `base` or its turn.
    fn open(
        kind: Codec,
        by_name: Option<&str>,
        base: (u32, u32),
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        // SAFETY: every pointer handed in is a live, NUL-terminated string or
        // a format/codec created just above and not yet freed.
        unsafe {
            let codec = match by_name {
                Some(n) => {
                    let n = CString::new(n).map_err(|e| e.to_string())?;
                    sys::AMediaCodec_createCodecByName(n.as_ptr())
                }
                None => sys::AMediaCodec_createEncoderByType(mime(kind).as_ptr()),
            };
            if codec.is_null() {
                return Err("no such encoder".into());
            }
            let mut raw: *mut c_char = null_mut();
            let name = if ok(sys::AMediaCodec_getName(codec, &mut raw)) && !raw.is_null() {
                let n = CStr::from_ptr(raw).to_string_lossy().into_owned();
                sys::AMediaCodec_releaseName(codec, raw);
                n
            } else {
                String::from("?")
            };
            let mut enc = Self {
                codec,
                hardware: !is_software(&name),
                name,
                by_name: by_name.map(str::to_string),
                kind,
                base,
                width,
                height,
                color: 0,
                stride: width as usize,
                slice_height: height as usize,
                pictures: 0,
                spent: Duration::ZERO,
                spent_n: 0,
                reported: Instant::now(),
            };
            // NV12 first: what hardware encoders take; I420 for the rest.
            let mut configured = false;
            for color in [COLOR_YUV420_SEMIPLANAR, COLOR_YUV420_PLANAR] {
                let f = Format::new();
                sys::AMediaFormat_setString(f.0, c"mime".as_ptr(), mime(kind).as_ptr());
                f.int(c"width", width as i32);
                f.int(c"height", height as i32);
                f.int(c"color-format", color);
                f.int(c"bitrate", bitrate(kind, width, height));
                f.int(c"frame-rate", video::FPS as i32);
                // Seconds between scheduled keyframes; requests come sooner.
                f.int(c"i-frame-interval", 4);
                // Realtime, not best-effort: a hint some encoders use to pick
                // their low-latency path.
                f.int(c"priority", 0);
                if ok(sys::AMediaCodec_configure(
                    codec,
                    f.0,
                    null_mut(),
                    null_mut(),
                    CONFIGURE_FLAG_ENCODE,
                )) {
                    enc.color = color;
                    configured = true;
                    break;
                }
            }
            if !configured {
                return Err(format!("{} would not take a 4:2:0 picture", enc.name));
            }
            if !ok(sys::AMediaCodec_start(codec)) {
                return Err(format!("{} would not start", enc.name));
            }
            // The layout the codec actually wants: its rows may be padded
            // (`stride`) and its planes spaced (`slice-height`).
            let input = Format(sys::AMediaCodec_getInputFormat(codec));
            if !input.0.is_null() {
                if let Some(c) = input.get_int(c"color-format") {
                    enc.color = c;
                }
                if let Some(s) = input.get_int(c"stride").filter(|&s| s >= width as i32) {
                    enc.stride = s as usize;
                }
                if let Some(s) = input
                    .get_int(c"slice-height")
                    .filter(|&s| s >= height as i32)
                {
                    enc.slice_height = s as usize;
                }
            }
            if enc.color != COLOR_YUV420_SEMIPLANAR && enc.color != COLOR_YUV420_PLANAR {
                return Err(format!(
                    "{} wants colour format {}, which this app does not write",
                    enc.name, enc.color
                ));
            }
            Ok(enc)
        }
    }

    /// Write `frame` into an input buffer in the codec's layout. `None` if
    /// the buffer is too small for what the layout says.
    fn fill(&self, buf: &mut [u8], frame: &I420) -> Option<usize> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (stride, slice) = (self.stride, self.slice_height);
        let luma = stride * slice;
        let need = if self.color == COLOR_YUV420_SEMIPLANAR {
            luma + stride * (ch - 1) + cw * 2
        } else {
            let cs = stride / 2;
            luma + cs * slice.div_ceil(2) + cs * (ch - 1) + cw
        };
        if buf.len() < need {
            return None;
        }
        for row in 0..h {
            buf[row * stride..row * stride + w].copy_from_slice(&frame.y[row * w..(row + 1) * w]);
        }
        if self.color == COLOR_YUV420_SEMIPLANAR {
            for row in 0..ch {
                let dst = &mut buf[luma + row * stride..luma + row * stride + cw * 2];
                let (u, v) = (&frame.u[row * cw..], &frame.v[row * cw..]);
                for (x, pair) in dst.chunks_exact_mut(2).enumerate() {
                    pair[0] = u[x];
                    pair[1] = v[x];
                }
            }
        } else {
            let cs = stride / 2;
            let v_at = luma + cs * slice.div_ceil(2);
            for row in 0..ch {
                buf[luma + row * cs..luma + row * cs + cw]
                    .copy_from_slice(&frame.u[row * cw..(row + 1) * cw]);
                buf[v_at + row * cs..v_at + row * cs + cw]
                    .copy_from_slice(&frame.v[row * cw..(row + 1) * cw]);
            }
        }
        Some(need)
    }

    fn report(&mut self, took: Duration) {
        self.spent += took;
        self.spent_n += 1;
        if self.reported.elapsed() >= Duration::from_secs(5) {
            let mean = self.spent.as_secs_f64() * 1e3 / f64::from(self.spent_n.max(1));
            tracing::info!(name = %self.name, pictures = self.spent_n, mean_ms = format!("{mean:.1}"), "call video: encoding");
            self.spent = Duration::ZERO;
            self.spent_n = 0;
            self.reported = Instant::now();
        }
    }
}

impl VideoEncode for MediaEncoder {
    fn encode(&mut self, frame: &I420, key: bool) -> Result<Vec<Vec<u8>>, Error> {
        let started = Instant::now();
        let (w, h) = video::oriented(self.base, frame.width, frame.height);
        let mut key = key;
        if (w, h) != (self.width, self.height) {
            // The phone turned: a codec is configured for one size, so a new
            // one, whose first picture is a keyframe.
            *self = Self::open(self.kind, self.by_name.as_deref(), self.base, w, h)
                .map_err(Error::new)?;
            key = true;
        }
        let scaled;
        let frame = if (frame.width, frame.height) == (w, h) {
            frame
        } else {
            scaled = frame.scaled(w, h);
            &scaled
        };
        // SAFETY: `self.codec` is live; buffers are used only between their
        // dequeue and the matching queue/release, at the sizes the codec gave.
        unsafe {
            if key && self.pictures > 0 {
                let f = Format::new();
                f.int(c"request-sync", 0);
                sys::AMediaCodec_setParameters(self.codec, f.0);
            }
            let idx =
                sys::AMediaCodec_dequeueInputBuffer(self.codec, INPUT_WAIT.as_micros() as i64);
            if idx >= 0 {
                let mut size = 0usize;
                let ptr = sys::AMediaCodec_getInputBuffer(self.codec, idx as usize, &mut size);
                if ptr.is_null() {
                    return Err(Error::new("no input buffer"));
                }
                let buf = std::slice::from_raw_parts_mut(ptr, size);
                let Some(used) = self.fill(buf, frame) else {
                    return Err(Error::new(format!(
                        "{}'s input buffer is {size} bytes, too small",
                        self.name
                    )));
                };
                let pts = self.pictures * 1_000_000 / u64::from(video::FPS);
                if !ok(sys::AMediaCodec_queueInputBuffer(
                    self.codec,
                    idx as usize,
                    0,
                    used,
                    pts,
                    0,
                )) {
                    return Err(Error::new("the codec refused a picture"));
                }
                self.pictures += 1;
            }
            // Whatever the codec has finished: usually this picture's frame.
            let mut out = Vec::new();
            let mut wait = OUTPUT_WAIT.as_micros() as i64;
            loop {
                let mut info = sys::AMediaCodecBufferInfo {
                    offset: 0,
                    size: 0,
                    presentationTimeUs: 0,
                    flags: 0,
                };
                let idx = sys::AMediaCodec_dequeueOutputBuffer(self.codec, &mut info, wait);
                if idx == INFO_TRY_AGAIN_LATER {
                    break;
                }
                if idx < 0 {
                    // The output format or buffers changed: nothing to read.
                    continue;
                }
                wait = 0;
                let mut size = 0usize;
                let ptr = sys::AMediaCodec_getOutputBuffer(self.codec, idx as usize, &mut size);
                let (start, len) = (info.offset.max(0) as usize, info.size.max(0) as usize);
                if !ptr.is_null()
                    && info.flags & BUFFER_FLAG_CODEC_CONFIG == 0
                    && len > 0
                    && start + len <= size
                {
                    out.push(std::slice::from_raw_parts(ptr.add(start), len).to_vec());
                }
                sys::AMediaCodec_releaseOutputBuffer(self.codec, idx as usize, false);
            }
            self.report(started.elapsed());
            Ok(out)
        }
    }

    fn codec(&self) -> Codec {
        self.kind
    }

    fn picture_size(&self) -> (u32, u32) {
        self.base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn googles_codecs_are_software_and_a_chip_makers_are_not() {
        assert!(is_software("c2.android.vp9.encoder"));
        assert!(is_software("OMX.google.vp8.encoder"));
        assert!(!is_software("c2.qti.vp8.encoder"));
        assert!(!is_software("c2.exynos.vp9.encoder"));
    }
}
