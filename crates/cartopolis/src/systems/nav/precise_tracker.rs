//! The high-precision mode's tracker, on a thread of its own, and where its
//! hand is on the map (`systems::high_precision`, `docs/notes/high-precision-mode.md`).
//!
//! [`cartopolis_vio::Tracker`] says how the phone has moved since it locked, to
//! the centimetre, in a frame of its own: z up, heading wherever the phone
//! pointed. [`Anchor`] puts that frame on the map once, at the lock — the GNSS
//! fix for where, the compass for which way, hand height above the ground for
//! how high — and from then on the hand is the anchor plus the tracker's
//! motion. Good to metres on the map and to centimetres relative to itself,
//! which for an instrument nobody can see is what matters: a theremin a metre
//! off is the same theremin.
//!
//! **The fix is read once, not followed.** Following it would hand the hand the
//! GNSS's metre of jitter every second, which is exactly what the tracker is
//! there to remove. A restart of the tracker (lost corners, a new scale)
//! re-anchors where the last pose left off, not on a fresh fix, so the hand
//! does not jump.

use crate::platform::sync::{Arc, Mutex};
use std::sync::mpsc::{self, Receiver, Sender};

use bevy::math::{DQuat, DVec3};
use cartopolis_vio::geom::Camera;
use cartopolis_vio::image::Gray;
use cartopolis_vio::{Imu, Pose, State, Tracker};

/// Where the phone is held above the ground when the tracker locks: a hand
/// held out in front of the chest.
pub const HAND_HEIGHT_M: f32 = 1.3;
/// Horizontal field of view assumed for a camera that does not report its
/// intrinsics — a phone's main camera is 65–80°.
const ASSUMED_HFOV_DEG: f64 = 72.0;

/// What the tracker thread is fed.
enum Feed {
    Imu(i64, Imu),
    Frame(i64, Gray),
}

/// What the tracker thread last concluded.
#[derive(Debug, Clone, Copy, Default)]
pub struct Latest {
    pub state: Option<State>,
    pub pose: Option<Pose>,
    pub epoch: u32,
    /// Frames the thread skipped because a newer one had arrived.
    pub dropped: u64,
}

/// The tracker running on its own thread. Dropping it ends the thread.
pub struct TrackerThread {
    tx: Sender<Feed>,
    latest: Arc<Mutex<Latest>>,
    camera: Camera,
}

impl TrackerThread {
    /// Start a tracker for frames `width × height` from a lens.
    #[must_use]
    pub fn start(lens: &cartopolis_core::lens::Lens, width: u32, height: u32) -> (Self, bool) {
        let (camera, reported) = camera_for(lens, width, height);
        let (tx, rx) = mpsc::channel();
        let latest = Arc::new(Mutex::new(Latest::default()));
        let out = latest.clone();
        let spawned = std::thread::Builder::new()
            .name("cartopolis-precise-track".into())
            .spawn(move || run(Tracker::new(camera), &rx, &out));
        if let Err(e) = spawned {
            tracing::warn!(%e, "could not start the tracker thread");
        }
        (Self { tx, latest, camera }, reported)
    }

    pub fn imu(&self, t_ns: i64, reading: Imu) {
        let _ = self.tx.send(Feed::Imu(t_ns, reading));
    }

    pub fn frame(&self, t_ns: i64, image: Gray) {
        let _ = self.tx.send(Feed::Frame(t_ns, image));
    }

    #[must_use]
    pub fn latest(&self) -> Latest {
        *self.latest.lock().unwrap()
    }

    #[must_use]
    pub fn camera(&self) -> &Camera {
        &self.camera
    }
}

/// The tracker thread: every motion reading, but only the newest frame of
/// whatever has queued up — a frame the tracker is too slow for is skipped,
/// never queued behind.
fn run(mut tracker: Tracker, rx: &Receiver<Feed>, out: &Mutex<Latest>) {
    while let Ok(first) = rx.recv() {
        let mut newest: Option<(i64, Gray)> = None;
        let mut dropped = 0;
        for feed in std::iter::once(first).chain(rx.try_iter()) {
            match feed {
                Feed::Imu(t, r) => tracker.imu(t, r),
                Feed::Frame(t, g) => {
                    dropped += u64::from(newest.is_some());
                    newest = Some((t, g));
                }
            }
        }
        let Some((t, image)) = newest else {
            continue;
        };
        let state = tracker.frame(t, image);
        let mut latest = out.lock().unwrap();
        latest.state = Some(state);
        latest.pose = tracker.pose();
        latest.epoch = tracker.epoch();
        latest.dropped += dropped;
    }
}

/// The tracker's camera for a lens: its own intrinsics where the phone reports
/// them, else a typical field of view. The flag says which.
#[must_use]
pub fn camera_for(lens: &cartopolis_core::lens::Lens, width: u32, height: u32) -> (Camera, bool) {
    match lens.pinhole(width, height) {
        Some(p) => (
            Camera::back(
                f64::from(p.fx),
                f64::from(p.fy),
                f64::from(p.cx),
                f64::from(p.cy),
                width,
                height,
                lens.orientation_deg,
            ),
            true,
        ),
        None => {
            let f = f64::from(width) / 2.0 / (ASSUMED_HFOV_DEG.to_radians() / 2.0).tan();
            (
                Camera::back(
                    f,
                    f,
                    f64::from(width) / 2.0,
                    f64::from(height) / 2.0,
                    width,
                    height,
                    lens.orientation_deg,
                ),
                false,
            )
        }
    }
}

/// The tracker's frame placed on the map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// The tracker epoch this anchor belongs to.
    pub epoch: u32,
    /// Where the tracker's `origin` is: latitude, longitude, metres above ground.
    pub lat: f64,
    pub lng: f64,
    pub height: f32,
    /// The tracker position that sits at that spot.
    pub origin: DVec3,
    /// Radians to turn the tracker's world about z to make it east-north-up.
    pub yaw: f64,
}

/// The compass bearing of where the back camera looks, radians from east
/// towards north (east-north-up), if it looks enough sideways to have one.
#[must_use]
pub fn enu_yaw(direction: DVec3) -> Option<f64> {
    (direction.truncate().length() > 0.3).then(|| direction.y.atan2(direction.x))
}

impl Anchor {
    /// Anchor a tracker pose at a map spot, given where the camera points
    /// according to the tracker and according to the compass (both as
    /// east-north-up-style vectors: the tracker's own world, and ENU).
    #[must_use]
    pub fn new(
        epoch: u32,
        pose: &Pose,
        camera: &Camera,
        at: (f64, f64, f32),
        compass_direction: DVec3,
    ) -> Option<Self> {
        let tracker_direction = pose.orientation * camera.device_from_camera * DVec3::Z;
        let yaw = enu_yaw(compass_direction)? - enu_yaw(tracker_direction)?;
        Some(Self {
            epoch,
            lat: at.0,
            lng: at.1,
            height: at.2,
            origin: pose.position,
            yaw,
        })
    }

    /// The same frame re-anchored for a new tracker epoch, so that `pose` —
    /// the new epoch's first — lands where `was` (the old epoch's last hand)
    /// was. Keeps the heading: a restart loses position, not north.
    #[must_use]
    pub fn continue_at(&self, epoch: u32, pose: &Pose, was: (f64, f64, f32)) -> Self {
        Self {
            epoch,
            lat: was.0,
            lng: was.1,
            height: was.2,
            origin: pose.position,
            ..*self
        }
    }

    /// Where a tracker position is on the map: latitude, longitude, metres
    /// above ground.
    #[must_use]
    pub fn place(&self, position: DVec3) -> (f64, f64, f32) {
        let enu = DQuat::from_rotation_z(self.yaw) * (position - self.origin);
        let (per_lat, per_lng) = cartopolis_core::degree_scale_m(self.lat);
        (
            self.lat + enu.y / per_lat,
            self.lng + enu.x / per_lng,
            self.height + enu.z as f32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        Camera::back(437.0, 437.0, 320.0, 240.0, 640, 480, 90)
    }

    /// A pose held upright in portrait, the back camera looking along the
    /// tracker's +x.
    fn pose_looking_along_x(position: DVec3) -> Pose {
        // Device x → world −y, device y → world z, device z → world −x: the
        // back (−z) looks along +x.
        let q = DQuat::from_mat3(&bevy::math::DMat3::from_cols(
            DVec3::NEG_Y,
            DVec3::Z,
            DVec3::NEG_X,
        ));
        Pose {
            position,
            orientation: q,
            t_ns: 0,
        }
    }

    #[test]
    fn a_step_forward_lands_where_the_compass_points() {
        let cam = camera();
        let start = pose_looking_along_x(DVec3::new(0.3, -0.2, 0.0));
        // The compass says the camera looks north.
        let anchor = Anchor::new(1, &start, &cam, (53.2, 6.58, 1.3), DVec3::Y).unwrap();
        // One metre forward in the tracker (+x) is one metre north.
        let (lat, lng, h) = anchor.place(start.position + DVec3::X);
        let (per_lat, per_lng) = cartopolis_core::degree_scale_m(53.2);
        assert!(((lat - 53.2) * per_lat - 1.0).abs() < 1e-6, "{lat}");
        assert!(((lng - 6.58) * per_lng).abs() < 1e-6);
        assert!((h - 1.3).abs() < 1e-6);
        // Raising the phone 20 cm raises the hand.
        let (_, _, h) = anchor.place(start.position + DVec3::Z * 0.2);
        assert!((h - 1.5).abs() < 1e-6);
    }

    #[test]
    fn a_restart_continues_where_the_hand_was() {
        let cam = camera();
        let first = pose_looking_along_x(DVec3::ZERO);
        let anchor = Anchor::new(1, &first, &cam, (53.2, 6.58, 1.3), DVec3::X).unwrap();
        let was = anchor.place(DVec3::new(0.5, 0.0, 0.1));
        let restarted = pose_looking_along_x(DVec3::new(7.0, 7.0, 7.0));
        let again = anchor.continue_at(2, &restarted, was);
        assert_eq!(again.place(restarted.position), was);
        assert_eq!(again.yaw, anchor.yaw);
    }

    #[test]
    fn a_phone_pointed_at_the_ground_has_no_heading() {
        assert!(enu_yaw(DVec3::new(0.1, 0.1, -0.99)).is_none());
        assert!(enu_yaw(DVec3::new(0.0, 1.0, 0.0)).is_some());
    }

    #[test]
    fn a_lens_without_intrinsics_gets_a_typical_field_of_view() {
        let (cam, reported) = camera_for(&cartopolis_core::lens::Lens::default(), 640, 480);
        assert!(!reported);
        let hfov = 2.0 * (320.0 / cam.fx).atan();
        assert!((hfov.to_degrees() - ASSUMED_HFOV_DEG).abs() < 1e-6);
    }
}
