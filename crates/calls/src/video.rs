//! Video for calls: VP9 through `rusty_vp9` and VP8 through `oxideav-vp8`,
//! both pure Rust, on every target.
//!
//! The app sends VP9 and decodes both; a browser sends VP9 too where it can
//! encode it, else VP8, and decodes both (`docs/notes/calls.md`). Pure Rust rather than libvpx
//! because it has no C: the same encoder and decoder run on the desktop, the
//! phone and in wasm. Measured before each was taken (the note has the
//! numbers): `oxideav-vp8` decodes a libvpx realtime stream bit-identically to
//! ffmpeg and encodes 320×240 in ~40 ms on this VPS; `rusty_vp9` is bit-exact
//! against libvpx's conformance vectors and encodes the same picture in
//! ~25 ms at well under half the bytes. Neither keeps up at 640×480 on a CPU,
//! which is why calls send 320×240 for now.
//!
//! Frames are I420 (planar 4:2:0), what both codecs code and what cameras and
//! decoders speak; RGBA only at the end, for display.

use oxideav_vp8::{
    I420Frame, KeyframeParams, TrellisStrength, Vp8DecoderState, Vp8InterStreamEncoder,
};
use rusty_vp9::{Vp9Decoder, Vp9Encoder, Vp9EncoderConfig};

use crate::media::Codec;

/// What a call sends: small enough that the pure-Rust encoder keeps up on a
/// phone's CPU, big enough to see a face.
pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 240;
/// Frames a second a call sends.
pub const FPS: u32 = 15;

/// Largest picture a receiver decodes: a hostile or broken stream must not be
/// able to make it allocate without bound. 1280×720.
pub const MAX_PIXELS: u64 = 1280 * 720;

/// A full picture every four seconds, so a receiver that lost one recovers
/// without asking; one on request ([`Encoder::encode`]'s `key`) sooner. A
/// keyframe is several times the bytes of the pictures around it, and lost
/// ones are repaired on request, so the schedule only has to bound the wait.
const KEYFRAME_INTERVAL: u64 = 4 * FPS as u64;
/// VP8 quantiser: measured 39 dB luma PSNR at ~0.5 Mbit/s at 30 fps, 320×240.
const QINDEX: u8 = 60;
/// VP9 constant quality (0..63): the same luma PSNR as VP8 at [`QINDEX`] on
/// the measured clip, at ~40 % of its bytes.
const VP9_CRF: u32 = 34;

/// One picture, planar 4:2:0, tightly packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct I420 {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

/// One plane of a picture as a camera hands it over: the bytes, how far apart
/// its rows are, and how far apart its samples are within a row.
#[derive(Clone, Copy, Debug)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub row_stride: usize,
    pub pixel_stride: usize,
}

fn chroma(width: u32, height: u32) -> (usize, usize) {
    (width.div_ceil(2) as usize, height.div_ceil(2) as usize)
}

impl I420 {
    /// A black picture.
    #[must_use]
    pub fn black(width: u32, height: u32) -> Self {
        let (cw, ch) = chroma(width, height);
        Self {
            width,
            height,
            y: vec![16; (width * height) as usize],
            u: vec![128; cw * ch],
            v: vec![128; cw * ch],
        }
    }

    /// A picture every byte of which the caller is about to write: zeroed
    /// pages, which the allocator hands over without touching them, rather
    /// than [`I420::black`]'s fill.
    fn zeroed(width: u32, height: u32) -> Self {
        let (cw, ch) = chroma(width, height);
        Self {
            width,
            height,
            y: vec![0; (width * height) as usize],
            u: vec![0; cw * ch],
            v: vec![0; cw * ch],
        }
    }

    /// From packed YUYV (YUY2), the uncompressed format nearly every webcam
    /// offers. 4:2:2 to 4:2:0 keeps the even rows' chroma.
    #[must_use]
    pub fn from_yuyv(width: u32, height: u32, yuyv: &[u8]) -> Option<Self> {
        let (w, h) = (width as usize, height as usize);
        if w % 2 != 0 || yuyv.len() < w * h * 2 {
            return None;
        }
        let mut out = Self::zeroed(width, height);
        let (cw, _) = chroma(width, height);
        for row in 0..h {
            let src = &yuyv[row * w * 2..(row + 1) * w * 2];
            for (i, px) in src.chunks_exact(4).enumerate() {
                out.y[row * w + 2 * i] = px[0];
                out.y[row * w + 2 * i + 1] = px[2];
                if row % 2 == 0 {
                    out.u[(row / 2) * cw + i] = px[1];
                    out.v[(row / 2) * cw + i] = px[3];
                }
            }
        }
        Some(out)
    }

    /// From NV12: a luma plane, then U and V interleaved at quarter size —
    /// the other uncompressed format webcams commonly offer.
    #[must_use]
    pub fn from_nv12(width: u32, height: u32, nv12: &[u8]) -> Option<Self> {
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = chroma(width, height);
        if nv12.len() < w * h + cw * ch * 2 {
            return None;
        }
        let mut out = Self::zeroed(width, height);
        out.y.copy_from_slice(&nv12[..w * h]);
        for (i, uv) in nv12[w * h..w * h + cw * ch * 2].chunks_exact(2).enumerate() {
            out.u[i] = uv[0];
            out.v[i] = uv[1];
        }
        Some(out)
    }

    /// From three separate planes, each with its own row stride and (for the
    /// chroma) pixel stride: Android's `YUV_420_888`, which is planar or
    /// semi-planar depending on the device and says which only through the
    /// strides. `None` if a plane is too short for what the strides claim.
    #[must_use]
    pub fn from_planes(
        width: u32,
        height: u32,
        y: Plane<'_>,
        u: Plane<'_>,
        v: Plane<'_>,
    ) -> Option<Self> {
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = chroma(width, height);
        let mut out = Self::zeroed(width, height);
        let copy = |p: Plane<'_>, pw: usize, ph: usize, dst: &mut [u8]| -> Option<()> {
            if ph == 0 || pw == 0 {
                return Some(());
            }
            let last = (ph - 1) * p.row_stride + (pw - 1) * p.pixel_stride;
            if p.data.len() <= last {
                return None;
            }
            for row in 0..ph {
                let src = &p.data[row * p.row_stride..];
                let dst = &mut dst[row * pw..(row + 1) * pw];
                if p.pixel_stride == 1 {
                    dst.copy_from_slice(&src[..pw]);
                } else {
                    for (x, d) in dst.iter_mut().enumerate() {
                        *d = src[x * p.pixel_stride];
                    }
                }
            }
            Some(())
        };
        copy(y, w, h, &mut out.y)?;
        copy(u, cw, ch, &mut out.u)?;
        copy(v, cw, ch, &mut out.v)?;
        Some(out)
    }

    /// Turned clockwise by `degrees` (0, 90, 180 or 270; anything else is
    /// taken as 0), then mirrored left-to-right if `mirror` — a phone camera's
    /// picture upright, and a front camera's the way a mirror shows it.
    #[must_use]
    pub fn rotated(&self, degrees: u32, mirror: bool) -> Self {
        let quarter = match degrees % 360 {
            90 => 1,
            180 => 2,
            270 => 3,
            _ => 0,
        };
        let (w, h) = if quarter % 2 == 1 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        let mut out = Self::zeroed(w, h);
        // For each destination pixel, the source pixel it comes from.
        let plane = |src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize| {
            for y in 0..dh {
                for x in 0..dw {
                    let (sx, sy) = match quarter {
                        1 => (y, sh - 1 - x),
                        2 => (sw - 1 - x, sh - 1 - y),
                        3 => (sw - 1 - y, x),
                        _ => (x, y),
                    };
                    let dx = if mirror { dw - 1 - x } else { x };
                    dst[y * dw + dx] = src[sy * sw + sx];
                }
            }
        };
        let (sw, sh) = (self.width as usize, self.height as usize);
        plane(&self.y, sw, sh, &mut out.y, w as usize, h as usize);
        let (scw, sch) = chroma(self.width, self.height);
        let (dcw, dch) = chroma(w, h);
        plane(&self.u, scw, sch, &mut out.u, dcw, dch);
        plane(&self.v, scw, sch, &mut out.v, dcw, dch);
        out
    }

    /// Scaled to `width`×`height` by nearest neighbour — a camera's picture
    /// to the call's size. Crude, and enough for a face at 320×240.
    #[must_use]
    pub fn scaled(&self, width: u32, height: u32) -> Self {
        if (width, height) == (self.width, self.height) {
            return self.clone();
        }
        let mut out = Self::zeroed(width, height);
        let plane = |src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize| {
            for y in 0..dh {
                let sy = y * sh / dh;
                for x in 0..dw {
                    dst[y * dw + x] = src[sy * sw + x * sw / dw];
                }
            }
        };
        let (sw, sh) = (self.width as usize, self.height as usize);
        let (dw, dh) = (width as usize, height as usize);
        plane(&self.y, sw, sh, &mut out.y, dw, dh);
        let (scw, sch) = chroma(self.width, self.height);
        let (dcw, dch) = chroma(width, height);
        plane(&self.u, scw, sch, &mut out.u, dcw, dch);
        plane(&self.v, scw, sch, &mut out.v, dcw, dch);
        out
    }

    /// RGBA for display, BT.601 limited range — what VP8 and VP9 streams
    /// carry. Opaque, so it is premultiplied as it stands.
    #[must_use]
    pub fn to_rgba(&self) -> Vec<u8> {
        self.to_rgba_mirrored(false)
    }

    /// [`I420::to_rgba`], mirrored left-to-right if `mirror`: the person's own
    /// view of themselves, without a mirrored copy of the picture first.
    #[must_use]
    pub fn to_rgba_mirrored(&self, mirror: bool) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let (cw, _) = chroma(self.width, self.height);
        let mut out = vec![0u8; w * h * 4];
        for (row, line) in out.chunks_exact_mut(w * 4).enumerate().take(h) {
            let ys = &self.y[row * w..(row + 1) * w];
            let us = &self.u[(row / 2) * cw..];
            let vs = &self.v[(row / 2) * cw..];
            for (col, &y) in ys.iter().enumerate() {
                let y = 298 * (i32::from(y) - 16);
                let u = i32::from(us[col / 2]) - 128;
                let v = i32::from(vs[col / 2]) - 128;
                let dst = if mirror { w - 1 - col } else { col };
                let px = &mut line[dst * 4..dst * 4 + 4];
                px[0] = ((y + 409 * v + 128) >> 8).clamp(0, 255) as u8;
                px[1] = ((y - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
                px[2] = ((y + 516 * u + 128) >> 8).clamp(0, 255) as u8;
                px[3] = 255;
            }
        }
        out
    }
}

#[derive(Debug, thiserror::Error)]
#[error("video: {0}")]
pub struct Error(String);

impl Error {
    #[must_use]
    pub fn new(why: impl Into<String>) -> Self {
        Self(why.into())
    }
}

/// Something that turns a call's pictures into VP8 or VP9 frames: this
/// module's [`Encoder`], or a platform's own (the phone's system codecs,
/// `platform::hw_video` in the app). Runs on the call's encoder thread.
pub trait VideoEncode: Send {
    /// Encode one picture; `key` forces a keyframe. Usually one frame comes
    /// back, but a codec with a pipeline may hand back none yet, or two.
    fn encode(&mut self, frame: &I420, key: bool) -> Result<Vec<Vec<u8>>, Error>;
    /// The codec of what [`VideoEncode::encode`] last returned.
    fn codec(&self) -> Codec;
    /// The size this encoder sends, landscape way up ([`oriented`] turns it
    /// with the picture): [`WIDTH`]×[`HEIGHT`] in software, more where a
    /// hardware encoder has the room. The camera is told, so it hands over
    /// pictures already this size rather than bigger ones to shrink.
    fn picture_size(&self) -> (u32, u32) {
        (WIDTH, HEIGHT)
    }
}

/// A hardware encoder's size: four times the pixels of the software one, which
/// a phone's encoder does in a few ms where the pure-Rust ones take ~100
/// (`docs/notes/calls.md`). Also what the phone's camera is asked for, so it
/// is a shrink-free picture.
pub const HW_WIDTH: u32 = 640;
pub const HW_HEIGHT: u32 = 480;

/// `size` (landscape) turned to match a `width`×`height` picture: an upright
/// phone sends portrait rather than a portrait squashed into landscape.
#[must_use]
pub fn oriented(size: (u32, u32), width: u32, height: u32) -> (u32, u32) {
    if height > width {
        (size.1, size.0)
    } else {
        size
    }
}

impl VideoEncode for Encoder {
    fn encode(&mut self, frame: &I420, key: bool) -> Result<Vec<Vec<u8>>, Error> {
        Encoder::encode(self, frame, key).map(|f| vec![f])
    }
    fn codec(&self) -> Codec {
        self.codec
    }
}

/// A platform encoder with this module's [`Encoder`] behind it: the first
/// error from the platform's drops it for good, and the stream carries on in
/// software from a keyframe. A phone's codec can fail in ways no test here
/// reaches (a vendor quirk, the codec reclaimed by the system), and a call
/// whose video stopped is worse than one whose video got slower.
pub struct WithFallback {
    primary: Option<Box<dyn VideoEncode>>,
    software: Option<Encoder>,
}

impl WithFallback {
    #[must_use]
    pub fn new(primary: Option<Box<dyn VideoEncode>>) -> Self {
        Self {
            primary,
            software: None,
        }
    }
}

impl VideoEncode for WithFallback {
    fn encode(&mut self, frame: &I420, key: bool) -> Result<Vec<Vec<u8>>, Error> {
        let mut key = key;
        if let Some(p) = self.primary.as_mut() {
            match p.encode(frame, key) {
                Ok(out) => return Ok(out),
                Err(e) => {
                    tracing::warn!(%e, "call video: the platform encoder failed; encoding in software");
                    self.primary = None;
                    key = true;
                }
            }
        }
        let soft = self.software.get_or_insert_with(Encoder::new);
        VideoEncode::encode(soft, frame, key)
    }
    fn codec(&self) -> Codec {
        self.primary.as_ref().map_or(Codec::Vp9, |p| p.codec())
    }
    fn picture_size(&self) -> (u32, u32) {
        self.primary
            .as_ref()
            .map_or((WIDTH, HEIGHT), |p| p.picture_size())
    }
}

/// The software encoder's size for `frame`: [`WIDTH`]×[`HEIGHT`], turned with
/// the picture ([`oriented`]).
#[must_use]
pub fn call_size(width: u32, height: u32) -> (u32, u32) {
    oriented((WIDTH, HEIGHT), width, height)
}

enum EncoderInner {
    Vp8(Vp8InterStreamEncoder),
    Vp9(Box<Vp9Encoder>),
}

/// One sender's encoder, VP9 unless asked for VP8.
///
/// VP9 through `rusty_vp9`, pure Rust like the VP8 one. Measured 2026-10-04 on
/// the VPS (`docs/notes/calls.md`): at 320×240 and the same luma PSNR it
/// encodes in ~25 ms against VP8's ~45 ms and sends ~180 kbit/s against
/// ~420 kbit/s at 15 fps. Half the bitrate is the point on a phone's uplink,
/// and the faster encode is battery.
pub struct Encoder {
    inner: EncoderInner,
    codec: Codec,
    width: u32,
    height: u32,
    /// Pictures since the last keyframe.
    since_key: u64,
}

impl Encoder {
    /// A VP9 encoder: what this app sends.
    #[must_use]
    pub fn new() -> Self {
        Self::with_codec(Codec::Vp9)
    }

    /// An encoder for `codec`, which must be a video codec (VP8 otherwise).
    #[must_use]
    pub fn with_codec(codec: Codec) -> Self {
        Self::sized(codec, WIDTH, HEIGHT)
    }

    fn sized(codec: Codec, width: u32, height: u32) -> Self {
        let (inner, codec) = match codec {
            Codec::Vp9 => (EncoderInner::Vp9(Box::new(vp9_encoder())), Codec::Vp9),
            _ => {
                let params = KeyframeParams {
                    y_ac_qi: QINDEX,
                    loop_filter_level: 20,
                    // Trellis quantisation buys a few percent of bitrate for a
                    // large share of the encode time, which is the budget that
                    // is short.
                    trellis_strength: TrellisStrength::OFF,
                    ..KeyframeParams::default()
                };
                let enc = Vp8InterStreamEncoder::new(params, KEYFRAME_INTERVAL)
                    .expect("a nonzero keyframe interval");
                (EncoderInner::Vp8(enc), Codec::Vp8)
            }
        };
        Self {
            inner,
            codec,
            width,
            height,
            since_key: 0,
        }
    }

    /// The codec this encoder writes.
    #[must_use]
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Encode one picture, scaled to the call's size first if it is not.
    /// `key` forces a full picture — the answer to a receiver's request.
    ///
    /// The call's size turns with the picture ([`call_size`]). A turn starts
    /// the stream again with a full picture, which is what both codecs need to
    /// change size and what every receiver's decoder already follows.
    pub fn encode(&mut self, frame: &I420, key: bool) -> Result<Vec<u8>, Error> {
        let size = call_size(frame.width, frame.height);
        let mut key = if size == (self.width, self.height) {
            key
        } else {
            *self = Self::sized(self.codec, size.0, size.1);
            true
        };
        let scaled;
        let frame = if (frame.width, frame.height) == size {
            frame
        } else {
            scaled = frame.scaled(size.0, size.1);
            &scaled
        };
        match &mut self.inner {
            EncoderInner::Vp8(enc) => {
                let f = I420Frame::packed(frame.width, frame.height, &frame.y, &frame.u, &frame.v);
                enc.encode_frame_with_force(&f, key)
                    .map(|e| e.bytes)
                    .map_err(|e| Error(format!("{e:?}")))
            }
            EncoderInner::Vp9(enc) => {
                key |= self.since_key >= KEYFRAME_INTERVAL;
                if key {
                    // `rusty_vp9` has no "force a keyframe": its first picture
                    // is one, and a fresh encoder costs nothing to make.
                    **enc = vp9_encoder();
                    self.since_key = 0;
                }
                self.since_key += 1;
                let (cw, _) = chroma(frame.width, frame.height);
                enc.push_frame(
                    [&frame.y, &frame.u, &frame.v],
                    [frame.width as usize, cw, cw],
                    frame.width,
                    frame.height,
                )
                .map_err(|e| Error(format!("{e:?}")))?;
                // No lookahead, so exactly one packet per picture.
                enc.next_packet()
                    .map(|p| p.data)
                    .map_err(|e| Error(format!("{e:?}")))
            }
        }
    }
}

/// VP9 for a call: no lookahead (a hidden alt-ref is a picture of delay), the
/// fastest speed preset, and a constant quality that measured the same luma
/// PSNR as VP8 at [`QINDEX`] for well under half the bytes.
fn vp9_encoder() -> Vp9Encoder {
    let mut enc = Vp9Encoder::default();
    enc.configure(&Vp9EncoderConfig {
        crf: Some(VP9_CRF),
        lag: Some(0),
        speed: Some(6),
        fps: Some(f64::from(FPS)),
        ..Default::default()
    })
    .expect("a valid VP9 configuration");
    enc
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

enum DecoderInner {
    Vp8(Vp8DecoderState),
    Vp9(Box<Vp9Decoder>),
}

/// One sender's decoder, as heard by one receiver. Follows the sender's codec:
/// a frame in the other one starts a fresh decoder (and so needs a keyframe).
pub struct Decoder {
    inner: DecoderInner,
}

impl Decoder {
    /// A decoder for VP8; it switches on the first VP9 frame it is handed.
    #[must_use]
    pub fn new() -> Self {
        Self::with_codec(Codec::Vp8)
    }

    #[must_use]
    pub fn with_codec(codec: Codec) -> Self {
        let inner = match codec {
            Codec::Vp9 => DecoderInner::Vp9(Box::default()),
            _ => DecoderInner::Vp8(Vp8DecoderState::new().with_max_pixels_per_frame(MAX_PIXELS)),
        };
        Self { inner }
    }

    #[must_use]
    pub fn codec(&self) -> Codec {
        match self.inner {
            DecoderInner::Vp8(_) => Codec::Vp8,
            DecoderInner::Vp9(_) => Codec::Vp9,
        }
    }

    /// Decode one VP8 frame. See [`Decoder::decode_as`].
    pub fn decode(&mut self, frame: &[u8]) -> Result<I420, Error> {
        self.decode_as(Codec::Vp8, frame)
    }

    /// Decode one frame in `codec`. An error usually means a reference frame
    /// was lost; the caller asks the sender for a keyframe.
    pub fn decode_as(&mut self, codec: Codec, frame: &[u8]) -> Result<I420, Error> {
        if codec != self.codec() {
            *self = Self::with_codec(codec);
        }
        match &mut self.inner {
            DecoderInner::Vp8(dec) => {
                let f = dec
                    .decode_frame(frame)
                    .map_err(|e| Error(format!("{e:?}")))?;
                Ok(I420 {
                    width: f.width,
                    height: f.height,
                    y: f.y,
                    u: f.u,
                    v: f.v,
                })
            }
            DecoderInner::Vp9(dec) => {
                // `rusty_vp9` takes the size a header names on trust, so a
                // hostile one is refused before it can allocate.
                let header = rusty_vp9::parse_uncompressed_header(
                    &mut rusty_vp9::BitReader::new(frame),
                    &[(0, 0); 8],
                )
                .map_err(|e| Error(format!("{e:?}")))?;
                if u64::from(header.width) * u64::from(header.height) > MAX_PIXELS {
                    return Err(Error(format!(
                        "{}×{} is larger than a call sends",
                        header.width, header.height
                    )));
                }
                dec.push(frame, None).map_err(|e| Error(format!("{e:?}")))?;
                let mut shown = None;
                loop {
                    match dec.next_frame() {
                        Ok(f) => shown = Some(f),
                        Err(rusty_vp9::Error::Again) => break,
                        Err(e) => return Err(Error(format!("{e:?}"))),
                    }
                }
                let f = shown.ok_or_else(|| Error("no picture shown".into()))?;
                if f.bit_depth != 8 || (f.subsampling_x, f.subsampling_y) != (1, 1) {
                    return Err(Error("not 8-bit 4:2:0".into()));
                }
                let (cw, ch) = chroma(f.width, f.height);
                let plane = |p: usize, w: usize, h: usize| -> Vec<u8> {
                    let stride = f.strides[p];
                    if stride == w {
                        f.planes[p][..w * h].to_vec()
                    } else {
                        (0..h)
                            .flat_map(|row| &f.planes[p][row * stride..row * stride + w])
                            .copied()
                            .collect()
                    }
                };
                Ok(I420 {
                    width: f.width,
                    height: f.height,
                    y: plane(0, f.width as usize, f.height as usize),
                    u: plane(1, cw, ch),
                    v: plane(2, cw, ch),
                })
            }
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// A stand-in camera for testing with no camera (`docs/notes/calls.md`): a
/// moving picture that says which frame it is, so a capture of the receiving
/// end shows that frames are arriving, in order, and with the colours right.
///
/// Eight vertical colour bars scroll one bar a second; below them a white
/// block steps across the width once per 16 frames; and the frame number is
/// written in binary as sixteen black-or-white squares along the bottom.
#[must_use]
pub fn test_pattern(n: u64) -> I420 {
    // (Y, U, V) of white, yellow, cyan, green, magenta, red, blue, black.
    const BARS: [(u8, u8, u8); 8] = [
        (235, 128, 128),
        (210, 16, 146),
        (170, 166, 16),
        (145, 54, 34),
        (106, 202, 222),
        (81, 90, 240),
        (41, 240, 110),
        (16, 128, 128),
    ];
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let mut f = I420::black(WIDTH, HEIGHT);
    let (cw, _) = chroma(WIDTH, HEIGHT);
    let shift = (n * w as u64 / (8 * u64::from(FPS))) as usize;
    let bars_h = h * 2 / 3;
    for row in 0..h {
        for col in 0..w {
            let (y, u, v) = if row < bars_h {
                BARS[((col + shift) % w) * 8 / w]
            } else if row < h - 20 {
                let block = (n % 16) as usize * w / 16;
                if (block..block + w / 16).contains(&col) {
                    (235, 128, 128)
                } else {
                    (16, 128, 128)
                }
            } else {
                let bit = col * 16 / w;
                if (n >> (15 - bit)) & 1 == 1 {
                    (235, 128, 128)
                } else {
                    (16, 128, 128)
                }
            };
            f.y[row * w + col] = y;
            if row % 2 == 0 && col % 2 == 0 {
                f.u[(row / 2) * cw + col / 2] = u;
                f.v[(row / 2) * cw + col / 2] = v;
            }
        }
    }
    f
}

/// Read back the frame number [`test_pattern`] wrote, from a decoded picture.
/// Samples the middle of each square, so compression noise does not matter.
#[must_use]
pub fn test_pattern_number(f: &I420) -> Option<u64> {
    if (f.width, f.height) != (WIDTH, HEIGHT) {
        return None;
    }
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let row = h - 10;
    let mut n = 0u64;
    for bit in 0..16 {
        let col = bit * w / 16 + w / 32;
        n = (n << 1) | u64::from(f.y[row * w + col] > 128);
    }
    Some(n)
}

/// A stand-in microphone to go with [`test_pattern`]: a 440 Hz tone, 20 ms at
/// a time, continuing from frame `n`.
#[must_use]
pub fn test_tone(rate: u32, n: u64) -> Vec<f32> {
    let len = crate::opus::frame_samples(rate);
    (0..len)
        .map(|i| {
            let t = (n as f32 * len as f32 + i as f32) / rate as f32;
            0.3 * (t * 440.0 * std::f32::consts::TAU).sin()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4×2 picture whose luma counts 0..8 row by row.
    fn counting() -> I420 {
        let mut p = I420::black(4, 2);
        p.y = (0..8).collect();
        p.u = vec![1, 2];
        p.v = vec![3, 4];
        p
    }

    #[test]
    fn a_quarter_turn_puts_the_top_left_at_the_top_right() {
        let p = counting();
        // 0 1 2 3      turned clockwise:   4 0
        // 4 5 6 7                          5 1
        //                                  6 2
        //                                  7 3
        let r = p.rotated(90, false);
        assert_eq!((r.width, r.height), (2, 4));
        assert_eq!(r.y, vec![4, 0, 5, 1, 6, 2, 7, 3]);
        assert_eq!(r.u, vec![1, 2]);
        assert_eq!(p.rotated(270, false).y, vec![3, 7, 2, 6, 1, 5, 0, 4]);
        assert_eq!(p.rotated(180, false).y, vec![7, 6, 5, 4, 3, 2, 1, 0]);
        assert_eq!(p.rotated(0, true).y, vec![3, 2, 1, 0, 7, 6, 5, 4]);
        // Four quarter turns are nothing, and so is a mirrored quarter turn
        // done twice (the mirror reverses the second turn).
        let mut q = p.clone();
        for _ in 0..4 {
            q = q.rotated(90, false);
        }
        assert_eq!(q, p);
        assert_eq!(p.rotated(90, true).rotated(90, true), p);
    }

    #[test]
    fn a_stream_turns_with_the_phone() {
        for codec in [Codec::Vp8, Codec::Vp9] {
            let mut enc = Encoder::with_codec(codec);
            let mut dec = Decoder::new();
            let upright = test_pattern(0).rotated(90, false);
            for (n, picture) in [test_pattern(0), test_pattern(1), upright.clone(), upright]
                .iter()
                .enumerate()
            {
                let bytes = enc.encode(picture, n == 0).unwrap();
                let got = dec.decode_as(codec, &bytes).unwrap();
                let want = if n < 2 {
                    (WIDTH, HEIGHT)
                } else {
                    (HEIGHT, WIDTH)
                };
                assert_eq!((got.width, got.height), want, "{codec:?} frame {n}");
            }
        }
    }

    #[test]
    fn the_app_sends_vp9_and_a_decoder_follows_a_sender_that_changes_codec() {
        assert_eq!(Encoder::new().codec(), Codec::Vp9);
        let mut dec = Decoder::new();
        let mut vp8 = Encoder::with_codec(Codec::Vp8);
        let mut vp9 = Encoder::new();
        let a = vp8.encode(&test_pattern(1), true).unwrap();
        assert_eq!(
            test_pattern_number(&dec.decode_as(Codec::Vp8, &a).unwrap()),
            Some(1)
        );
        let b = vp9.encode(&test_pattern(2), false).unwrap();
        assert!(
            Codec::Vp9.is_keyframe(&b),
            "a VP9 stream starts on a keyframe"
        );
        assert_eq!(
            test_pattern_number(&dec.decode_as(Codec::Vp9, &b).unwrap()),
            Some(2)
        );
        assert_eq!(dec.codec(), Codec::Vp9);
    }

    #[test]
    fn a_vp9_header_naming_a_huge_picture_is_refused_before_it_allocates() {
        let mut enc = Encoder::new();
        let mut key = enc.encode(&test_pattern(0), true).unwrap();
        // Profile 0 keyframe: byte 0, sync code (3), then 4 bits of colour
        // config, then width-1 and height-1 as 16-bit fields. Set both to
        // 0xffff by OR-ing ones into the bits they occupy.
        let start_bit = 8 + 24 + 4;
        for bit in start_bit..start_bit + 32 {
            key[bit / 8] |= 0x80 >> (bit % 8);
        }
        assert!(Decoder::with_codec(Codec::Vp9)
            .decode_as(Codec::Vp9, &key)
            .is_err());
    }

    #[test]
    fn strided_planes_are_read_as_planar_or_interleaved() {
        // Luma padded to a row stride of 6; chroma interleaved (pixel stride 2)
        // the way semi-planar devices hand it over, V one byte after U.
        let y = [0, 1, 2, 3, 99, 99, 4, 5, 6, 7, 99, 99];
        let uv = [10, 20, 11, 21];
        let p = I420::from_planes(
            4,
            2,
            Plane {
                data: &y,
                row_stride: 6,
                pixel_stride: 1,
            },
            Plane {
                data: &uv,
                row_stride: 4,
                pixel_stride: 2,
            },
            Plane {
                data: &uv[1..],
                row_stride: 4,
                pixel_stride: 2,
            },
        )
        .unwrap();
        assert_eq!(p.y, (0..8).collect::<Vec<u8>>());
        assert_eq!((p.u, p.v), (vec![10, 11], vec![20, 21]));
        // A plane shorter than its strides claim is refused, not read past.
        let short = Plane {
            data: &y[..7],
            row_stride: 6,
            pixel_stride: 1,
        };
        let c = Plane {
            data: &uv,
            row_stride: 4,
            pixel_stride: 2,
        };
        assert!(I420::from_planes(4, 2, short, c, c).is_none());
    }

    #[test]
    fn the_test_pattern_survives_the_codec_and_says_which_frame_it_is() {
        for codec in [Codec::Vp8, Codec::Vp9] {
            let mut enc = Encoder::with_codec(codec);
            let mut dec = Decoder::with_codec(codec);
            // Past the scheduled keyframe, so the stream restarts once.
            for n in 0..(KEYFRAME_INTERVAL + 5) {
                let sent = test_pattern(n);
                let got = dec
                    .decode_as(codec, &enc.encode(&sent, false).unwrap())
                    .unwrap();
                assert_eq!((got.width, got.height), (WIDTH, HEIGHT));
                assert_eq!(test_pattern_number(&got), Some(n), "{codec:?} frame {n}");
            }
        }
    }

    #[test]
    fn a_forced_keyframe_decodes_without_what_came_before() {
        for codec in [Codec::Vp8, Codec::Vp9] {
            let mut enc = Encoder::with_codec(codec);
            for n in 0..5 {
                let delta = enc.encode(&test_pattern(n), false).unwrap();
                assert_eq!(codec.is_keyframe(&delta), n == 0, "{codec:?} frame {n}");
            }
            let key = enc.encode(&test_pattern(5), true).unwrap();
            assert!(
                codec.is_keyframe(&key),
                "{codec:?} frame header says keyframe"
            );
            let got = Decoder::with_codec(codec).decode_as(codec, &key).unwrap();
            assert_eq!(test_pattern_number(&got), Some(5), "{codec:?}");
        }
    }

    /// A platform encoder that works twice and then breaks, as a phone codec
    /// reclaimed mid-call would.
    struct Flaky {
        inner: Encoder,
        left: u32,
    }

    impl VideoEncode for Flaky {
        fn encode(&mut self, frame: &I420, key: bool) -> Result<Vec<Vec<u8>>, Error> {
            if self.left == 0 {
                return Err(Error::new("codec reclaimed"));
            }
            self.left -= 1;
            VideoEncode::encode(&mut self.inner, frame, key)
        }
        fn codec(&self) -> Codec {
            Codec::Vp8
        }
        fn picture_size(&self) -> (u32, u32) {
            (HW_WIDTH, HW_HEIGHT)
        }
    }

    #[test]
    fn a_failing_platform_encoder_hands_over_to_software_on_a_keyframe() {
        let mut enc = WithFallback::new(Some(Box::new(Flaky {
            inner: Encoder::with_codec(Codec::Vp8),
            left: 2,
        })));
        let mut dec = Decoder::new();
        assert_eq!(enc.picture_size(), (HW_WIDTH, HW_HEIGHT));
        for n in 0..5u64 {
            // What the camera hands over while the hardware size is asked
            // for; software shrinks it once it has taken over.
            let picture = test_pattern(n).scaled(HW_WIDTH, HW_HEIGHT);
            let out = enc.encode(&picture, n == 0).unwrap();
            let codec = enc.codec();
            let size = if n < 2 {
                (HW_WIDTH, HW_HEIGHT)
            } else {
                (WIDTH, HEIGHT)
            };
            assert_eq!(enc.picture_size(), size, "frame {n}");
            assert_eq!(
                codec,
                if n < 2 { Codec::Vp8 } else { Codec::Vp9 },
                "frame {n}"
            );
            assert_eq!(out.len(), 1);
            if n == 2 {
                assert!(
                    codec.is_keyframe(&out[0]),
                    "the switch starts on a keyframe"
                );
            }
            let got = dec.decode_as(codec, &out[0]).unwrap();
            assert_eq!(test_pattern_number(&got), Some(n));
        }
    }

    #[test]
    fn a_mirrored_conversion_is_the_mirror_of_the_plain_one() {
        let f = test_pattern(3);
        let (plain, mirrored) = (f.to_rgba(), f.to_rgba_mirrored(true));
        let w = WIDTH as usize;
        for row in [0, 100, HEIGHT as usize - 1] {
            for col in [0, 7, w - 1] {
                let a = &plain[(row * w + col) * 4..][..4];
                let b = &mirrored[(row * w + (w - 1 - col)) * 4..][..4];
                assert_eq!(a, b, "row {row} col {col}");
            }
        }
        assert_eq!(f.rotated(0, true).to_rgba(), mirrored);
    }

    #[test]
    fn colour_comes_out_the_right_way_round() {
        let f = test_pattern(0);
        let rgba = f.to_rgba();
        // First bar is white, second yellow (red + green, no blue).
        let at = |col: usize| &rgba[(10 * WIDTH as usize + col) * 4..][..3];
        assert!(at(5).iter().all(|&c| c > 240), "{:?}", at(5));
        let yellow = at(WIDTH as usize / 8 + 5);
        assert!(
            yellow[0] > 200 && yellow[1] > 200 && yellow[2] < 40,
            "{yellow:?}"
        );
    }

    #[test]
    fn a_webcam_yuyv_picture_becomes_i420() {
        // 4×2 of mid grey with distinct chroma per pair.
        let yuyv = [
            100, 90, 101, 200, 102, 91, 103, 201, 110, 0, 111, 0, 112, 0, 113, 0,
        ];
        let f = I420::from_yuyv(4, 2, &yuyv).unwrap();
        assert_eq!(f.y, vec![100, 101, 102, 103, 110, 111, 112, 113]);
        assert_eq!(f.u, vec![90, 91]);
        assert_eq!(f.v, vec![200, 201]);
        assert!(I420::from_yuyv(3, 2, &yuyv).is_none());
    }

    #[test]
    fn a_webcam_nv12_picture_becomes_i420() {
        let nv12 = [1, 2, 3, 4, 5, 6, 7, 8, 90, 200, 91, 201];
        let f = I420::from_nv12(4, 2, &nv12).unwrap();
        assert_eq!(f.y, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(f.u, vec![90, 91]);
        assert_eq!(f.v, vec![200, 201]);
        assert!(I420::from_nv12(4, 2, &nv12[..10]).is_none());
    }
}
