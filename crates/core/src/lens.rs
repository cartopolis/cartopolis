//! What a phone's camera says about its own lens, read from its Camera2
//! characteristics (`platform::camera::run_back_gray`) for the high-precision
//! mode's localiser (`docs/notes/high-precision-mode.md`).
//!
//! In `cartopolis_core` because the localiser is Bevy-free and is developed
//! and tested off the phone, against rendered views with a lens of known
//! intrinsics; it needs the same type the phone fills in.

use serde::{Deserialize, Serialize};

/// A camera's lens, as Android reports it. Every field but the orientation is
/// optional on Android, and a missing one is `None` rather than a guess.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Lens {
    /// `LENS_INTRINSIC_CALIBRATION`: `[fx, fy, cx, cy, skew]` in pixels of the
    /// **active array**, not of a delivered frame — [`Lens::pinhole`] scales.
    pub intrinsics: Option<[f32; 5]>,
    /// `LENS_DISTORTION`: `[k1, k2, k3, p1, p2]`.
    pub distortion: Option<[f32; 5]>,
    /// `LENS_POSE_ROTATION` (a quaternion `x, y, z, w`) and
    /// `LENS_POSE_TRANSLATION` (metres): where the camera sits relative to
    /// [`Self::pose_reference`] — the gyroscope, on most phones.
    pub pose_rotation: Option<[f32; 4]>,
    pub pose_translation: Option<[f32; 3]>,
    /// `LENS_POSE_REFERENCE`: 0 primary camera, 1 gyroscope, 2 undefined,
    /// 3 automotive.
    pub pose_reference: Option<u8>,
    /// `SENSOR_INFO_ACTIVE_ARRAY_SIZE`: `[left, top, right, bottom]`.
    pub active_array: Option<[i32; 4]>,
    /// `SENSOR_ORIENTATION`: degrees to turn a frame to stand it upright with
    /// the phone held in its natural orientation.
    pub orientation_deg: u32,
    /// `SENSOR_INFO_TIMESTAMP_SOURCE` is `REALTIME`: frame stamps are on the
    /// motion sensors' clock (`CLOCK_BOOTTIME`). `false` means they are not,
    /// and fusing the two needs the offset estimated.
    pub realtime_timestamps: bool,
    /// `LENS_INFO_AVAILABLE_FOCAL_LENGTHS`, the first: millimetres. With
    /// [`Self::physical_size_mm`] it gives the field of view on a phone that
    /// publishes no calibration (the OnePlus 15 publishes none).
    #[serde(default)]
    pub focal_length_mm: Option<f32>,
    /// `SENSOR_INFO_PHYSICAL_SIZE`: the active sensor's width and height, mm.
    #[serde(default)]
    pub physical_size_mm: Option<[f32; 2]>,
}

/// A pinhole camera in the pixels of a delivered frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pinhole {
    pub fx: f32,
    pub fy: f32,
    pub cx: f32,
    pub cy: f32,
}

impl Lens {
    /// The intrinsics for a frame `width × height`, scaled from the active
    /// array the device reports them in. The frame is assumed to be the whole
    /// active array scaled (Camera2's default when no crop is asked for), with
    /// any aspect difference cropped symmetrically.
    #[must_use]
    pub fn pinhole(&self, width: u32, height: u32) -> Option<Pinhole> {
        let [fx, fy, cx, cy, _] = self.intrinsics?;
        let [l, t, r, b] = self.active_array?;
        let (aw, ah) = ((r - l) as f32, (b - t) as f32);
        if aw <= 0.0 || ah <= 0.0 || width == 0 || height == 0 {
            return None;
        }
        // Scale to fit the frame's shorter side; the longer is cropped.
        let s = (width as f32 / aw).max(height as f32 / ah);
        let (dx, dy) = (
            (aw * s - width as f32) / 2.0,
            (ah * s - height as f32) / 2.0,
        );
        Some(Pinhole {
            fx: fx * s,
            fy: fy * s,
            cx: cx * s - dx,
            cy: cy * s - dy,
        })
    }

    /// The field of view of a delivered frame `width × height`, in the
    /// sensor's own orientation (before any turn to upright): `(horizontal,
    /// vertical)` in degrees. From the calibration when the phone publishes
    /// one, otherwise from the focal length and the sensor's size, under the
    /// same "scaled whole, longer side cropped" assumption as [`Self::pinhole`].
    #[must_use]
    pub fn field_of_view_deg(&self, width: u32, height: u32) -> Option<(f32, f32)> {
        if width == 0 || height == 0 {
            return None;
        }
        let (w, h) = (width as f32, height as f32);
        let angle = |extent: f32, focal: f32| 2.0 * (extent / 2.0 / focal).atan().to_degrees();
        if let Some(p) = self.pinhole(width, height) {
            if p.fx > 0.0 && p.fy > 0.0 {
                return Some((angle(w, p.fx), angle(h, p.fy)));
            }
        }
        let f = self.focal_length_mm.filter(|f| *f > 0.0)?;
        let [pw, ph] = self
            .physical_size_mm
            .filter(|[a, b]| *a > 0.0 && *b > 0.0)?;
        // Pixels per millimetre of sensor, fitting the shorter side.
        let s = (w / pw).max(h / ph);
        Some((angle(w / s, f), angle(h / s, f)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_field_of_view_comes_from_calibration_or_from_the_sensor() {
        // A 4 mm lens on a 6.4 × 4.8 mm sensor: 77.3° by 61.9°.
        let physical = Lens {
            focal_length_mm: Some(4.0),
            physical_size_mm: Some([6.4, 4.8]),
            ..Lens::default()
        };
        let (h, v) = physical.field_of_view_deg(1920, 1440).unwrap();
        assert!(
            (h - 77.32).abs() < 0.05 && (v - 61.93).abs() < 0.05,
            "{h} {v}"
        );
        // A 16:9 frame from the same 4:3 sensor keeps the width, loses height.
        let (h2, v2) = physical.field_of_view_deg(1920, 1080).unwrap();
        assert!((h2 - h).abs() < 0.05 && v2 < v);
        // Calibration wins: fx = fy = 1000 px on a 2000 × 1500 array.
        let calibrated = Lens {
            intrinsics: Some([1000.0, 1000.0, 1000.0, 750.0, 0.0]),
            active_array: Some([0, 0, 2000, 1500]),
            ..physical
        };
        let (h, v) = calibrated.field_of_view_deg(2000, 1500).unwrap();
        assert!(
            (h - 90.0).abs() < 0.01 && (v - 73.74).abs() < 0.05,
            "{h} {v}"
        );
        assert!(Lens::default().field_of_view_deg(640, 480).is_none());
    }

    #[test]
    fn intrinsics_scale_to_the_frame() {
        let lens = Lens {
            intrinsics: Some([3200.0, 3200.0, 2000.0, 1500.0, 0.0]),
            active_array: Some([0, 0, 4000, 3000]),
            ..Lens::default()
        };
        let p = lens.pinhole(640, 480).unwrap();
        assert!((p.fx - 512.0).abs() < 1e-3);
        assert!((p.cx - 320.0).abs() < 1e-3 && (p.cy - 240.0).abs() < 1e-3);
        // A 16:9 frame from a 4:3 array is cropped top and bottom.
        let p = lens.pinhole(640, 360).unwrap();
        assert!((p.cy - 180.0).abs() < 1e-3, "{p:?}");
        assert_eq!(Lens::default().pinhole(640, 480), None);
    }
}
