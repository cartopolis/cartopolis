//! Visual-inertial odometry for the high-precision mode
//! (`docs/notes/high-precision-mode.md`): how the phone moves, to the
//! centimetre, from its back camera and its gyroscope and accelerometer.
//!
//! **Relative, not absolute.** What a theremin needs is a steady hand: the
//! hand's movement relative to where tracking began, to a centimetre, held
//! for the minute somebody plays. Where that start sits on the map is the
//! GNSS fix and the compass's job, good to metres — which for an instrument
//! nobody can see is the same as exact. Matching the camera against the
//! building models to tighten that anchor is a later layer, not this one.
//!
//! The pieces, each Bevy-free and tested here against synthetic data with a
//! known answer:
//!
//! * [`image`] — FAST corners and pyramidal Lucas–Kanade tracking;
//! * [`attitude`] — the gyroscope integrated, the accelerometer holding up;
//! * [`odometry`] — the camera's path up to scale, from tracked corners with
//!   the rotation already known;
//! * [`scale`] — metres per visual unit, from the accelerometer.

pub mod attitude;
pub mod geom;
pub mod image;
pub mod odometry;
pub mod scale;
#[cfg(test)]
mod sim;

use glam::{DQuat, DVec3, Vec2};

use crate::attitude::Attitude;
use crate::geom::Camera;
use crate::image::{fast_corners, spread, track, Gray, Pyramid};
use crate::odometry::{Fix, Observation, Odometry};
use crate::scale::Scale;

/// Corners kept in view; new ones are found when fewer survive.
const TARGET_CORNERS: usize = 160;
/// One new corner per cell this size, at most.
const CORNER_CELL_PX: usize = 32;
/// FAST's brightness threshold.
const FAST_THRESHOLD: u8 = 18;
/// Pyramid levels: 640×480 down to 80×60.
const LEVELS: usize = 4;

/// A motion-sensor reading, in device axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Imu {
    /// m/s², gravity included.
    Accel(DVec3),
    /// rad/s.
    Gyro(DVec3),
}

/// Where the phone is, as far as the tracker knows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    /// Metres from where tracking began, in the tracker's world: z up, heading
    /// wherever the phone pointed at the start.
    pub position: DVec3,
    /// World from device.
    pub orientation: DQuat,
    /// Frame time, ns on the sensors' clock.
    pub t_ns: i64,
}

/// What the tracker is doing, for the mode's log and its status line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    /// No gravity yet, or no frame.
    Waiting,
    /// Following corners, waiting for the phone to move enough to start.
    Starting { corners: usize, parallax_deg: f64 },
    /// Tracking, up to scale; waiting for enough motion to know metres.
    Scaling { corners: usize, landmarks: usize },
    /// Tracking in metres.
    Tracking { corners: usize, landmarks: usize },
}

/// The whole tracker: feed it motion readings and frames, read a [`Pose`].
pub struct Tracker {
    cam: Camera,
    attitude: Attitude,
    /// Motion readings not yet applied: they may be newer than the next frame.
    pending: Vec<(i64, Imu)>,
    last_accel_ns: Option<i64>,
    prev: Option<Pyramid>,
    corners: Vec<(u64, Vec2)>,
    next_id: u64,
    odometry: Odometry,
    epoch: u32,
    scale: Scale,
    /// The visual position at the start of this epoch, so poses read from
    /// where tracking began.
    origin: Option<DVec3>,
    state: State,
    pose: Option<Pose>,
}

impl Tracker {
    #[must_use]
    pub fn new(cam: Camera) -> Self {
        Self {
            cam,
            attitude: Attitude::default(),
            pending: Vec::new(),
            last_accel_ns: None,
            prev: None,
            corners: Vec::new(),
            next_id: 0,
            odometry: Odometry::default(),
            epoch: 0,
            scale: Scale::default(),
            origin: None,
            state: State::Waiting,
            pose: None,
        }
    }

    /// A motion reading, in any order relative to frames.
    pub fn imu(&mut self, t_ns: i64, reading: Imu) {
        self.pending.push((t_ns, reading));
    }

    /// The camera's lens, in this tracker's frames.
    #[must_use]
    pub fn camera(&self) -> &Camera {
        &self.cam
    }

    /// Apply the motion readings up to `t_ns`, oldest first.
    fn catch_up(&mut self, t_ns: i64) {
        self.pending.sort_by_key(|(t, _)| *t);
        let split = self.pending.partition_point(|(t, _)| *t <= t_ns);
        let ready: Vec<(i64, Imu)> = self.pending.drain(..split).collect();
        for (t, reading) in ready {
            match reading {
                Imu::Gyro(w) => self.attitude.gyro(t, w),
                Imu::Accel(a) => {
                    let dt = self
                        .last_accel_ns
                        .map_or(0.005, |p| ((t - p) as f64 * 1e-9).clamp(0.0, 0.1));
                    self.last_accel_ns = Some(t);
                    self.attitude.accel(a, dt);
                    if self.attitude.started() {
                        self.scale
                            .accel(t as f64 * 1e-9, self.attitude.linear_acceleration(a));
                    }
                }
            }
        }
    }

    /// A camera frame: greyscale, as the sensor delivered it, stamped on the
    /// motion sensors' clock.
    pub fn frame(&mut self, t_ns: i64, image: Gray) -> State {
        let pyramid = Pyramid::new(image, LEVELS);
        if let Some(prev) = &self.prev {
            let points: Vec<Vec2> = self.corners.iter().map(|(_, p)| *p).collect();
            let tracked = track(prev, &pyramid, &points);
            self.corners = self
                .corners
                .iter()
                .zip(tracked)
                .filter_map(|((id, _), t)| t.map(|p| (*id, p)))
                .collect();
        }
        if self.corners.len() < TARGET_CORNERS * 3 / 4 {
            let fine = &pyramid.levels[0];
            let taken: Vec<Vec2> = self.corners.iter().map(|(_, p)| *p).collect();
            let found = fast_corners(fine, FAST_THRESHOLD, 12);
            for p in spread(
                &found,
                fine.w,
                fine.h,
                CORNER_CELL_PX,
                &taken,
                TARGET_CORNERS - self.corners.len(),
            ) {
                self.corners.push((self.next_id, p));
                self.next_id += 1;
            }
        }
        self.prev = Some(pyramid);
        let seen: Vec<Observation> = self
            .corners
            .iter()
            .map(|(id, px)| Observation { id: *id, px: *px })
            .collect();
        self.observe(t_ns, &seen)
    }

    /// The tracker's back half, from corners already followed: what a frame
    /// becomes after [`Self::frame`]'s image work. Public so the estimator can
    /// be tested against synthetic corners with no images at all.
    pub fn observe(&mut self, t_ns: i64, seen: &[Observation]) -> State {
        self.catch_up(t_ns);
        if !self.attitude.started() {
            self.state = State::Waiting;
            return self.state;
        }
        let world_from_camera = self.attitude.q * self.cam.device_from_camera;
        let corners = seen.len();
        self.state = match self.odometry.update(&self.cam, world_from_camera, seen) {
            Fix::Starting { parallax_deg, .. } => State::Starting {
                corners,
                parallax_deg,
            },
            Fix::Lost => {
                self.pose = None;
                State::Starting {
                    corners,
                    parallax_deg: 0.0,
                }
            }
            Fix::Located {
                position,
                landmarks,
                correction,
            } => {
                // The landmarks' word on orientation, back to the attitude.
                self.attitude.q = (correction * self.attitude.q).normalize();
                if self.odometry.epoch != self.epoch {
                    // A new start is a new visual scale.
                    self.epoch = self.odometry.epoch;
                    self.scale.reset();
                    self.origin = Some(position);
                }
                let t = t_ns as f64 * 1e-9;
                self.scale.frame(t, position);
                match self.scale.metres_per_unit() {
                    Some(s) => {
                        let origin = self.origin.unwrap_or(position);
                        self.pose = Some(Pose {
                            position: (position - origin) * s,
                            orientation: self.attitude.q,
                            t_ns,
                        });
                        State::Tracking { corners, landmarks }
                    }
                    None => State::Scaling { corners, landmarks },
                }
            }
        };
        self.state
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// The newest pose, while tracking in metres.
    #[must_use]
    pub fn pose(&self) -> Option<Pose> {
        matches!(self.state, State::Tracking { .. })
            .then_some(self.pose)
            .flatten()
    }

    /// Restarts so far — each one moves the origin. The caller anchors a new
    /// start where the old pose left off.
    #[must_use]
    pub fn epoch(&self) -> u32 {
        self.epoch
    }
}
