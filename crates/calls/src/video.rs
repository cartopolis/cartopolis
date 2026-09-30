//! Video for calls: VP8 through `oxideav-vp8`, pure Rust, on every target.
//!
//! VP8 because every participant must decode what every other sends and it is
//! the codec every device can encode (`docs/notes/calls.md`). `oxideav-vp8`
//! rather than libvpx because it has no C: the same encoder and decoder run on
//! the desktop, the phone and in wasm. Measured before it was taken (the note
//! has the numbers): it decodes a libvpx realtime stream bit-identically to
//! ffmpeg, ffmpeg decodes what it encodes bit-identically to itself, and at
//! 320×240 it encodes a frame in ~40 ms on this VPS under load — 15 fps with
//! room. At 640×480 it does not keep up, which is why calls send 320×240 for
//! now; a phone's hardware encoder is the way up, and the frames on the wire
//! would be the same.
//!
//! Frames are I420 (planar 4:2:0), what VP8 codes and what cameras and
//! decoders speak; RGBA only at the end, for display.

use oxideav_vp8::{
    I420Frame, KeyframeParams, TrellisStrength, Vp8DecoderState, Vp8InterStreamEncoder,
};

/// What a call sends: small enough that the pure-Rust encoder keeps up on a
/// phone's CPU, big enough to see a face.
pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 240;
/// Frames a second a call sends.
pub const FPS: u32 = 15;

/// Largest picture a receiver decodes: a hostile or broken stream must not be
/// able to make it allocate without bound. 1280×720.
pub const MAX_PIXELS: u64 = 1280 * 720;

/// A full picture every two seconds, so a receiver that lost one recovers
/// without asking; one on request ([`Encoder::encode`]'s `key`) sooner.
const KEYFRAME_INTERVAL: u64 = 2 * FPS as u64;
/// Quantiser: measured 39 dB luma PSNR at ~0.5 Mbit/s at 30 fps, 320×240.
const QINDEX: u8 = 60;

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

    /// From packed YUYV (YUY2), the uncompressed format nearly every webcam
    /// offers. 4:2:2 to 4:2:0 keeps the even rows' chroma.
    #[must_use]
    pub fn from_yuyv(width: u32, height: u32, yuyv: &[u8]) -> Option<Self> {
        let (w, h) = (width as usize, height as usize);
        if w % 2 != 0 || yuyv.len() < w * h * 2 {
            return None;
        }
        let mut out = Self::black(width, height);
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
        let mut out = Self::black(width, height);
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
        let mut out = Self::black(width, height);
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
        let mut out = Self::black(w, h);
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
        let mut out = Self::black(width, height);
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

    /// RGBA for display, BT.601 limited range — what VP8 streams carry.
    #[must_use]
    pub fn to_rgba(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let (cw, _) = chroma(self.width, self.height);
        let mut out = vec![0u8; w * h * 4];
        for row in 0..h {
            for col in 0..w {
                let y = 298 * (i32::from(self.y[row * w + col]) - 16);
                let u = i32::from(self.u[(row / 2) * cw + col / 2]) - 128;
                let v = i32::from(self.v[(row / 2) * cw + col / 2]) - 128;
                let px = &mut out[(row * w + col) * 4..][..4];
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

/// One sender's VP8 encoder.
pub struct Encoder {
    inner: Vp8InterStreamEncoder,
    width: u32,
    height: u32,
}

impl Encoder {
    #[must_use]
    pub fn new() -> Self {
        Self::sized(WIDTH, HEIGHT)
    }

    fn sized(width: u32, height: u32) -> Self {
        let params = KeyframeParams {
            y_ac_qi: QINDEX,
            loop_filter_level: 20,
            // Trellis quantisation buys a few percent of bitrate for a large
            // share of the encode time, which is the budget that is short.
            trellis_strength: TrellisStrength::OFF,
            ..KeyframeParams::default()
        };
        Self {
            inner: Vp8InterStreamEncoder::new(params, KEYFRAME_INTERVAL)
                .expect("a nonzero keyframe interval"),
            width,
            height,
        }
    }

    /// Encode one picture, scaled to the call's size first if it is not.
    /// `key` forces a full picture — the answer to a receiver's request.
    ///
    /// The call's size turns with the picture: an upright phone sends
    /// [`HEIGHT`]×[`WIDTH`], not a portrait squashed into landscape. A turn
    /// starts the stream again with a full picture, which is what VP8 needs to
    /// change size and what every receiver's decoder already follows.
    pub fn encode(&mut self, frame: &I420, key: bool) -> Result<Vec<u8>, Error> {
        let size = if frame.height > frame.width {
            (HEIGHT, WIDTH)
        } else {
            (WIDTH, HEIGHT)
        };
        let key = if size == (self.width, self.height) {
            key
        } else {
            *self = Self::sized(size.0, size.1);
            true
        };
        let frame = frame.scaled(self.width, self.height);
        let f = I420Frame::packed(frame.width, frame.height, &frame.y, &frame.u, &frame.v);
        self.inner
            .encode_frame_with_force(&f, key)
            .map(|e| e.bytes)
            .map_err(|e| Error(format!("{e:?}")))
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

/// One sender's VP8 decoder, as heard by one receiver.
pub struct Decoder {
    inner: Vp8DecoderState,
}

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Vp8DecoderState::new().with_max_pixels_per_frame(MAX_PIXELS),
        }
    }

    /// Decode one frame. An error usually means a reference frame was lost;
    /// the caller asks the sender for a keyframe.
    pub fn decode(&mut self, frame: &[u8]) -> Result<I420, Error> {
        let f = self
            .inner
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
        let mut enc = Encoder::new();
        let mut dec = Decoder::new();
        let upright = test_pattern(0).rotated(90, false);
        for (n, picture) in [test_pattern(0), test_pattern(1), upright.clone(), upright]
            .iter()
            .enumerate()
        {
            let bytes = enc.encode(picture, n == 0).unwrap();
            let got = dec.decode(&bytes).unwrap();
            let want = if n < 2 {
                (WIDTH, HEIGHT)
            } else {
                (HEIGHT, WIDTH)
            };
            assert_eq!((got.width, got.height), want, "frame {n}");
        }
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
        let mut enc = Encoder::new();
        let mut dec = Decoder::new();
        for n in 0..40u64 {
            let sent = test_pattern(n);
            let got = dec.decode(&enc.encode(&sent, false).unwrap()).unwrap();
            assert_eq!((got.width, got.height), (WIDTH, HEIGHT));
            assert_eq!(test_pattern_number(&got), Some(n), "frame {n}");
        }
    }

    #[test]
    fn a_forced_keyframe_decodes_without_what_came_before() {
        let mut enc = Encoder::new();
        for n in 0..5 {
            enc.encode(&test_pattern(n), false).unwrap();
        }
        let key = enc.encode(&test_pattern(5), true).unwrap();
        assert_eq!(key[0] & 1, 0, "VP8 frame tag says keyframe");
        let got = Decoder::new().decode(&key).unwrap();
        assert_eq!(test_pattern_number(&got), Some(5));
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
