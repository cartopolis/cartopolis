//! A face, followed through a camera: MediaPipe's Face Landmarker, run by
//! tract (`docs/notes/avatar-faces.md`).
//!
//! Three models, as MediaPipe chains them:
//! 1. **the detector** (BlazeFace, short range) finds a face in the whole
//!    picture, with its eyes — run only while no face is being followed;
//! 2. **the landmarker** reads 478 points off a square around the face,
//!    turned so the eyes are level, and says whether a face is still there;
//!    the next frame's square comes from these points, so a face that moves is
//!    followed without the detector;
//! 3. **the blend-shape model** turns 146 of those points into the 52 ARKit
//!    weights the avatar's face is driven by (`cartopolis_human::face::UNITS`).
//!
//! What MediaPipe does in its graph's calculators — the detector's anchors and
//! box decoding, the square, the turn, the crop, the points back into the
//! picture — is done here, from the calculators' documented behaviour. The
//! picture is read through [`Frame`], so a camera's own format (I420 on the
//! phone) is sampled where it lies rather than converted whole.

use std::sync::Arc;

use tract_tflite::prelude::*;

/// A picture to read a face from, upright: pixel `(0, 0)` top left, as the
/// person would see a photograph of themselves — not mirrored.
pub trait Frame {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    /// The colour at a pixel inside the picture.
    fn rgb(&self, x: u32, y: u32) -> [u8; 3];
}

/// A plain RGB picture, rows top to bottom.
pub struct Rgb<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
}

impl Frame for Rgb<'_> {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn rgb(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * self.width + x) * 3) as usize;
        [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2]]
    }
}

/// How many weights a face has, in `cartopolis_human::face::UNITS`' order.
pub const UNITS: usize = 52;

/// A followed face.
#[derive(Clone, Debug)]
pub struct Tracked {
    /// The 52 unit weights, 0–1. MediaPipe gives no `tongueOut`; it is 0.
    pub weights: [f32; UNITS],
    /// The 478 points, in the picture's pixels; `z` in the same scale,
    /// smaller toward the camera.
    pub landmarks: Vec<[f32; 3]>,
}

/// The landmarks at the centres of the right and the left iris.
const RIGHT_IRIS: usize = 468;
const LEFT_IRIS: usize = 473;

/// The usual gap between an adult's pupils, metres: 63 mm, the mean of
/// both sexes (spread about ±4 mm).
pub const PUPIL_GAP_M: f32 = 0.063;

impl Tracked {
    /// The gap between the pupils, in the picture's pixels.
    #[must_use]
    pub fn eye_gap(&self) -> f32 {
        match (
            self.landmarks.get(RIGHT_IRIS),
            self.landmarks.get(LEFT_IRIS),
        ) {
            (Some(r), Some(l)) => (l[0] - r[0]).hypot(l[1] - r[1]),
            _ => 0.0,
        }
    }

    /// How far the face is from the camera, metres, for a lens of
    /// `focal_px` (its focal length in this picture's pixels), from the pupil
    /// gap: right to a few centimetres at arm's length, since people's gaps
    /// differ by about 6 %. `None` when the eyes were not found.
    #[must_use]
    pub fn distance(&self, focal_px: f32) -> Option<f32> {
        let gap = self.eye_gap();
        (gap > 1.0).then(|| PUPIL_GAP_M * focal_px / gap)
    }
}

type Model = Arc<TypedRunnableModel>;

pub struct FaceTracker {
    detector: Model,
    landmarker: Model,
    blendshapes: Model,
    anchors: Vec<[f32; 2]>,
    /// The square the last frame's face was in, to look in again.
    roi: Option<Roi>,
}

/// A turned square on the picture, pixels: its centre, side and turn.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Roi {
    centre: [f32; 2],
    size: f32,
    /// Radians; positive turns the square's x axis toward the picture's +y.
    angle: f32,
}

impl Roi {
    /// A point of the square, `u`/`v` 0–1 across it, in the picture.
    fn to_picture(&self, u: f32, v: f32) -> [f32; 2] {
        let (s, c) = self.angle.sin_cos();
        let (x, y) = ((u - 0.5) * self.size, (v - 0.5) * self.size);
        [
            self.centre[0] + c * x - s * y,
            self.centre[1] + s * x + c * y,
        ]
    }
}

/// The detector's input side, pixels.
const DETECTOR_SIZE: usize = 128;
/// The landmarker's.
const LANDMARKER_SIZE: usize = 256;
/// How far round a face the square reaches: MediaPipe's 1.5.
const ROI_SCALE: f32 = 1.5;
/// A detection worth following, and a face still there.
const MIN_SCORE: f32 = 0.5;

/// The points the blend-shape model reads, MediaPipe's
/// `kLandmarksSubsetIdxs`.
const SUBSET: [usize; 146] = [
    0, 1, 4, 5, 6, 7, 8, 10, 13, 14, 17, 21, 33, 37, 39, 40, 46, 52, 53, 54, 55, 58, 61, 63, 65,
    66, 67, 70, 78, 80, 81, 82, 84, 87, 88, 91, 93, 95, 103, 105, 107, 109, 127, 132, 133, 136,
    144, 145, 146, 148, 149, 150, 152, 153, 154, 155, 157, 158, 159, 160, 161, 162, 163, 168, 172,
    173, 176, 178, 181, 185, 191, 195, 197, 234, 246, 249, 251, 263, 267, 269, 270, 276, 282, 283,
    284, 285, 288, 291, 293, 295, 296, 297, 300, 308, 310, 311, 312, 314, 317, 318, 321, 323, 324,
    332, 334, 336, 338, 356, 361, 362, 365, 373, 374, 375, 377, 378, 379, 380, 381, 382, 384, 385,
    386, 387, 388, 389, 390, 397, 398, 400, 402, 405, 409, 415, 454, 466, 468, 469, 470, 471, 472,
    473, 474, 475, 476, 477,
];

/// The landmarks whose line levels the square: the outer corners of the
/// right and the left eye.
const RIGHT_EYE: usize = 33;
const LEFT_EYE: usize = 263;

fn load(bytes: &[u8]) -> Result<Model, String> {
    tract_tflite::tflite()
        .model_for_read(&mut std::io::Cursor::new(bytes))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|e| format!("{e:#}"))
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x.clamp(-100.0, 100.0)).exp())
}

impl FaceTracker {
    /// From the three `.tflite` files of MediaPipe's `face_landmarker.task`.
    pub fn new(detector: &[u8], landmarker: &[u8], blendshapes: &[u8]) -> Result<Self, String> {
        Ok(Self {
            detector: load(detector)?,
            landmarker: load(landmarker)?,
            blendshapes: load(blendshapes)?,
            anchors: anchors(),
            roi: None,
        })
    }

    /// Lose the face followed, so the next frame looks for one afresh.
    pub fn forget(&mut self) {
        self.roi = None;
    }

    /// The face in `frame`, if there is one.
    pub fn track(&mut self, frame: &impl Frame) -> Option<Tracked> {
        let roi = match self.roi {
            Some(r) => r,
            None => self.detect(frame)?,
        };
        let Some(landmarks) = self.landmarks(frame, roi) else {
            self.roi = None;
            return None;
        };
        self.roi = Some(roi_from_landmarks(&landmarks));
        let weights = self.blend(&landmarks)?;
        Some(Tracked { weights, landmarks })
    }

    /// The most certain face in the whole picture, as a square to read.
    fn detect(&self, frame: &impl Frame) -> Option<Roi> {
        let (w, h) = (frame.width() as f32, frame.height() as f32);
        // Letterboxed into the square, as MediaPipe's `keep_aspect_ratio`.
        let side = w.max(h);
        let (ox, oy) = ((side - w) / 2.0, (side - h) / 2.0);
        let n = DETECTOR_SIZE;
        let input = tract_ndarray::Array4::from_shape_fn((1, n, n, 3), |(_, y, x, c)| {
            let px = (x as f32 + 0.5) / n as f32 * side - ox;
            let py = (y as f32 + 0.5) / n as f32 * side - oy;
            sample(frame, px, py).map_or(0.0, |rgb| rgb[c] / 127.5 - 1.0)
        });
        let out = self.detector.run(tvec!(Tensor::from(input).into())).ok()?;
        let boxes = out[0].to_plain_array_view::<f32>().ok()?;
        let scores = out[1].to_plain_array_view::<f32>().ok()?;
        let boxes = boxes.as_slice()?;
        let scores = scores.as_slice()?;
        let (best, score) = scores
            .iter()
            .enumerate()
            .map(|(i, s)| (i, sigmoid(*s)))
            .max_by(|a, b| a.1.total_cmp(&b.1))?;
        if score < MIN_SCORE {
            return None;
        }
        let raw = &boxes[best * 16..best * 16 + 16];
        let a = self.anchors[best];
        let scale = DETECTOR_SIZE as f32;
        // Detector space (0–1 across the letterboxed square) to pixels.
        let px = |x: f32, y: f32| [x * side - ox, y * side - oy];
        let centre = px(raw[0] / scale + a[0], raw[1] / scale + a[1]);
        let (bw, bh) = (raw[2] / scale * side, raw[3] / scale * side);
        let eye = |k: usize| px(raw[4 + 2 * k] / scale + a[0], raw[5 + 2 * k] / scale + a[1]);
        let (right, left) = (eye(0), eye(1));
        Some(Roi {
            centre,
            size: bw.max(bh) * ROI_SCALE,
            angle: level(right, left),
        })
    }

    /// The 478 points of the face in `roi`, in the picture; `None` when the
    /// landmarker sees no face there.
    fn landmarks(&self, frame: &impl Frame, roi: Roi) -> Option<Vec<[f32; 3]>> {
        let n = LANDMARKER_SIZE;
        let input = tract_ndarray::Array4::from_shape_fn((1, n, n, 3), |(_, y, x, c)| {
            let [px, py] = roi.to_picture((x as f32 + 0.5) / n as f32, (y as f32 + 0.5) / n as f32);
            sample(frame, px, py).map_or(0.0, |rgb| rgb[c] / 255.0)
        });
        let out = self
            .landmarker
            .run(tvec!(Tensor::from(input).into()))
            .ok()?;
        let presence = out[1].to_plain_array_view::<f32>().ok()?;
        if sigmoid(*presence.iter().next()?) < MIN_SCORE {
            return None;
        }
        let points = out[0].to_plain_array_view::<f32>().ok()?;
        let points = points.as_slice()?;
        let k = n as f32;
        Some(
            points
                .chunks_exact(3)
                .map(|p| {
                    let [x, y] = roi.to_picture(p[0] / k, p[1] / k);
                    [x, y, p[2] / k * roi.size]
                })
                .collect(),
        )
    }

    /// The 52 weights from the points.
    fn blend(&self, landmarks: &[[f32; 3]]) -> Option<[f32; UNITS]> {
        let input = tract_ndarray::Array3::from_shape_fn((1, SUBSET.len(), 2), |(_, i, c)| {
            landmarks.get(SUBSET[i]).map_or(0.0, |p| p[c])
        });
        let out = self
            .blendshapes
            .run(tvec!(Tensor::from(input).into()))
            .ok()?;
        let scores = out[0].to_plain_array_view::<f32>().ok()?;
        let scores = scores.as_slice()?;
        // MediaPipe's first is `_neutral`; the rest are the units in order,
        // less the last, `tongueOut`.
        let mut weights = [0.0; UNITS];
        for (w, s) in weights.iter_mut().zip(scores.iter().skip(1)) {
            *w = s.clamp(0.0, 1.0);
        }
        Some(weights)
    }
}

/// The turn that levels the line from the right eye to the left: MediaPipe's
/// `target_angle - atan2(-(y1 - y0), x1 - x0)`, with a target of 0.
fn level(right: [f32; 2], left: [f32; 2]) -> f32 {
    (left[1] - right[1]).atan2(left[0] - right[0])
}

/// The square for the next frame, round the points this one found.
fn roi_from_landmarks(points: &[[f32; 3]]) -> Roi {
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in points {
        for a in 0..2 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let at = |i: usize| points.get(i).map_or([0.0; 2], |p| [p[0], p[1]]);
    Roi {
        centre: [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0],
        size: (hi[0] - lo[0]).max(hi[1] - lo[1]) * ROI_SCALE,
        angle: level(at(RIGHT_EYE), at(LEFT_EYE)),
    }
}

/// The colour at a point of the picture, bilinear, 0–255; `None` outside it
/// (MediaPipe pads with zeros).
fn sample(frame: &impl Frame, x: f32, y: f32) -> Option<[f32; 3]> {
    let (w, h) = (frame.width() as f32, frame.height() as f32);
    let (x, y) = (x - 0.5, y - 0.5);
    if x < -0.5 || y < -0.5 || x > w - 0.5 || y > h - 0.5 {
        return None;
    }
    let (x, y) = (x.clamp(0.0, w - 1.0), y.clamp(0.0, h - 1.0));
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (x1, y1) = (
        (x0 + 1).min(frame.width() - 1),
        (y0 + 1).min(frame.height() - 1),
    );
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let [a, b, c, d] = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| frame.rgb(x, y));
    let mut out = [0.0; 3];
    for k in 0..3 {
        let top = f32::from(a[k]) * (1.0 - fx) + f32::from(b[k]) * fx;
        let bottom = f32::from(c[k]) * (1.0 - fx) + f32::from(d[k]) * fx;
        out[k] = top * (1.0 - fy) + bottom * fy;
    }
    Some(out)
}

/// The detector's 896 anchor centres, MediaPipe's `SsdAnchorsCalculator` for
/// the short-range model: strides 8, 16, 16, 16 over 128 pixels, the three
/// layers of stride 16 sharing one grid, two anchors a layer at each place,
/// every anchor 1 × 1 (`fixed_anchor_size`), so only the centres matter.
fn anchors() -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(896);
    for (stride, per_cell) in [(8usize, 2usize), (16, 6)] {
        let cells = DETECTOR_SIZE / stride;
        for y in 0..cells {
            for x in 0..cells {
                for _ in 0..per_cell {
                    out.push([
                        (x as f32 + 0.5) / cells as f32,
                        (y as f32 + 0.5) / cells as f32,
                    ]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_anchors_and_the_subset_are_mediapipes() {
        assert_eq!(anchors().len(), 896);
        assert!(SUBSET.windows(2).all(|w| w[0] < w[1]), "sorted, no repeats");
        assert_eq!(*SUBSET.last().unwrap(), 477);
    }

    /// A square's corners land where its turn puts them.
    #[test]
    fn a_turned_square_maps_its_corners() {
        let r = Roi {
            centre: [100.0, 50.0],
            size: 20.0,
            angle: std::f32::consts::FRAC_PI_2,
        };
        let [x, y] = r.to_picture(1.0, 0.5);
        assert!(
            (x - 100.0).abs() < 1e-4 && (y - 60.0).abs() < 1e-4,
            "{x} {y}"
        );
    }

    /// The shipped models, or `None` in a checkout without Git LFS (where
    /// each is a 130-byte pointer).
    fn shipped() -> Option<FaceTracker> {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/models/face");
        let read = |n: &str| {
            std::fs::read(format!("{dir}/{n}.tflite"))
                .ok()
                .filter(|b| b.len() > 4096)
        };
        Some(
            FaceTracker::new(
                &read("face_detector")?,
                &read("face_landmarks_detector")?,
                &read("face_blendshapes")?,
            )
            .expect("the shipped models load"),
        )
    }

    // Indices into `cartopolis_human::face::UNITS`.
    const EYE_BLINK_LEFT: usize = 8;
    const EYE_BLINK_RIGHT: usize = 9;
    const JAW_OPEN: usize = 24;
    const MOUTH_SMILE_LEFT: usize = 43;
    const MOUTH_SMILE_RIGHT: usize = 44;

    /// A broad smile reads as one, as TFLite's own interpreter reads it (0.92
    /// and 0.89 for the corners, measured 2026-10-27; tract's mean rounded to
    /// whole pixels until `crates/tract_tflite` was patched, and gave 1.0 and
    /// 0.0); the eyes are open; and the second frame, followed from the first
    /// frame's points without the detector, reads the same.
    #[test]
    fn a_smile_reads_as_a_smile() {
        let Some(mut t) = shipped() else { return };
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/smile.jpg");
        let Some(img) = image::open(path).ok().map(|i| i.to_rgb8()) else {
            return;
        };
        let frame = Rgb {
            width: img.width(),
            height: img.height(),
            pixels: img.as_raw(),
        };
        let face = t.track(&frame).expect("a face is found");
        assert_eq!(face.landmarks.len(), 478);
        let w = face.weights;
        assert!(
            w[MOUTH_SMILE_LEFT] > 0.8 && w[MOUTH_SMILE_RIGHT] > 0.8,
            "smile {} {}",
            w[MOUTH_SMILE_LEFT],
            w[MOUTH_SMILE_RIGHT]
        );
        assert!(w[EYE_BLINK_LEFT] < 0.4 && w[EYE_BLINK_RIGHT] < 0.4);
        assert!(
            w[JAW_OPEN] < 0.3,
            "lips apart, jaw not dropped: {}",
            w[JAW_OPEN]
        );
        // The nose tip (1) sits between the eyes' outer corners.
        let [nose, right, left] = [1, RIGHT_EYE, LEFT_EYE].map(|i| face.landmarks[i]);
        assert!(
            right[0] < nose[0] && nose[0] < left[0],
            "{right:?} {nose:?} {left:?}"
        );
        // The pupils lie a little inside the eyes' outer corners, and
        // a 300-pixel crop of a 500-pixel portrait puts them some 60 px apart.
        let gap = face.eye_gap();
        let corners = (left[0] - right[0]).abs();
        assert!(
            gap > 0.4 * corners && gap < corners,
            "gap {gap}, corners {corners}"
        );
        let again = t.track(&frame).expect("followed");
        assert!((again.weights[MOUTH_SMILE_LEFT] - w[MOUTH_SMILE_LEFT]).abs() < 0.1);
    }

    /// No face, no answer: a grey picture finds nothing.
    #[test]
    fn a_blank_picture_has_no_face() {
        let Some(mut t) = shipped() else { return };
        let pixels = vec![128u8; 200 * 150 * 3];
        let frame = Rgb {
            width: 200,
            height: 150,
            pixels: &pixels,
        };
        assert!(t.track(&frame).is_none());
    }
}
