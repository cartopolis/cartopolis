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
}

#[cfg(test)]
mod tests {
    use super::*;

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
