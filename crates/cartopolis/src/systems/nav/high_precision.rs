//! **High precision**: the camera mode on the locate button
//! (`docs/notes/high-precision-mode.md`). Long-press the button to switch it on.
//!
//! The camera is there for one thing: to pin down where the phone is, to
//! centimetres, by matching what it sees against the 3DBAG building models,
//! with the gyroscope and accelerometer carrying the pose between matches.
//! With that the view *is* the phone in your hand, which is what makes a
//! hand-sensing prim (`systems::prim_hand`, `on_hand`) playable — a theremin on
//! the Grote Markt. Nothing the camera sees is kept.
//!
//! **The matcher is not built yet.** Switched on today, the mode runs the
//! sensors and the locate button, and writes what they report to the
//! [`HighPrecisionLog`] — the lens calibration, sensor rates, frame brightness,
//! GNSS accuracy — which Developer tools ▸ Precision shows and copies, so a
//! phone's behaviour can be pasted into a chat. [`PreciseHand::active`] stays
//! `false` until a pose is actually held: a mode that claimed centimetres it
//! does not have would make every hand-sensing script lie.
//!
//! It turns the locate button on with it: the GNSS fix and the compass are the
//! box the matcher searches, and they are the button's.

use std::collections::VecDeque;

use bevy::prelude::*;

use crate::platform::precise_sensors::{Counts, MotionKind, Sensors};
use crate::systems::locate::LocateTracking;
use crate::systems::prim_hand::PreciseHand;

/// The mode, as the button and the systems see it.
#[derive(Resource, Default)]
pub struct HighPrecision {
    /// Asked for. The sensors follow it in [`drive_high_precision`].
    pub on: bool,
    sensors: Option<Sensors>,
    /// Seconds since the sensors started, and the counts at the last log line,
    /// for rates.
    running_s: f32,
    last_line_s: f32,
    last_counts: Counts,
    logged_lens: bool,
    logged_error: bool,
    /// The newest frame's size and brightness, for the log.
    last_frame: Option<(u32, u32, f32)>,
    /// How old the newest frame was when it was taken off the camera thread,
    /// in ms. Tens of ms means the camera's clock is the motion sensors' clock;
    /// anything wildly off means it is not, and the two cannot be fused as-is.
    frame_age_ms: Option<f64>,
    /// Since the last log line: the fastest turn the gyroscope saw (rad/s),
    /// and the mean length of the accelerometer's vector (≈ 9.81 m/s² when the
    /// phone is held still — a sanity check on the units and the sensor).
    gyro_peak: f32,
    accel_sum: f32,
    accel_n: u32,
    /// The longest gap between two gyroscope events since the last log line,
    /// in ms: a tracker integrates them, so a gap is motion nobody measured.
    last_gyro_t: Option<i64>,
    gyro_gap_ms: f64,
}

impl HighPrecision {
    /// Whether this device can do it at all — a phone, not a desktop or a browser.
    #[must_use]
    pub const fn available() -> bool {
        Sensors::available()
    }

    /// One line under the locate button saying what the mode is doing.
    #[must_use]
    pub fn status(&self) -> Option<String> {
        let sensors = self.sensors.as_ref()?;
        if let Some(e) = sensors.error() {
            return Some(e);
        }
        Some(if sensors.counts().frames == 0 {
            "High precision · starting camera".into()
        } else {
            "High precision · no position lock yet".into()
        })
    }
}

/// What the mode did, as text a person can copy off the phone and paste into a
/// chat: Developer tools ▸ Precision. Kept small on purpose — it is read by a
/// person, through a clipboard — and kept on the phone across launches.
#[derive(Resource)]
pub struct HighPrecisionLog {
    lines: VecDeque<String>,
    bytes: usize,
    dirty: bool,
}

/// The most the log holds; the oldest lines go first.
const LOG_MAX_BYTES: usize = 24 * 1024;
/// How often a running mode writes its rates to the log.
const LOG_EVERY_S: f32 = 5.0;
/// Where the log is kept between launches.
#[cfg(not(target_arch = "wasm32"))]
const LOG_FILE: &str = "high_precision.log";

impl Default for HighPrecisionLog {
    /// What the last launch left, so a log survives the app being closed
    /// between the walk and the copy.
    fn default() -> Self {
        let mut log = Self {
            lines: VecDeque::new(),
            bytes: 0,
            dirty: false,
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(text) = std::fs::read_to_string(log_path()) {
            for line in text.lines() {
                log.push_line(line.to_string());
            }
        }
        log.dirty = false;
        log
    }
}

// No filesystem on the web, and no camera there either: the log lives only in
// memory, which there it never needs to outlive.
#[cfg(not(target_arch = "wasm32"))]
fn log_path() -> std::path::PathBuf {
    crate::utils::cache::app_data_root().join(LOG_FILE)
}

impl HighPrecisionLog {
    fn push_line(&mut self, line: String) {
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        while self.bytes > LOG_MAX_BYTES {
            let Some(old) = self.lines.pop_front() else {
                break;
            };
            self.bytes -= old.len() + 1;
        }
        self.dirty = true;
    }

    /// Add a line, stamped with the wall clock (UTC).
    pub fn note(&mut self, text: impl AsRef<str>) {
        let line = format!("{} {}", utc_now(), text.as_ref());
        tracing::info!(target: "high_precision", "{line}");
        self.push_line(line);
    }

    /// The whole log, as it is copied.
    #[must_use]
    pub fn text(&self) -> String {
        let mut s = String::with_capacity(self.bytes);
        for l in &self.lines {
            s.push_str(l);
            s.push('\n');
        }
        s
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.bytes = 0;
        self.dirty = true;
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Write it out if it changed. Small, so a rewrite is cheaper than reasoning
    /// about appends and trims.
    fn save(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = log_path();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(&path, self.text()) {
                tracing::warn!(%e, "could not keep the high-precision log");
            }
        }
    }
}

/// `HH:MM:SS` of the wall clock, UTC — enough to line the log up with a walk.
fn utc_now() -> String {
    let secs = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let day = secs % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, day / 60 % 60, day % 60)
}

/// Run the sensors while the switch is on, and keep the log.
pub fn drive_high_precision(
    time: Res<Time>,
    mut high: ResMut<HighPrecision>,
    mut log: ResMut<HighPrecisionLog>,
    mut tracking: ResMut<LocateTracking>,
    mut precise: ResMut<PreciseHand>,
) {
    // No matcher yet: the view is not the hand, whatever the switch says.
    if precise.active {
        precise.active = false;
    }
    if !HighPrecision::available() {
        high.on = false;
        return;
    }
    let high = &mut *high;
    match (high.on, high.sensors.is_some()) {
        (true, false) => {
            tracking.enter();
            high.sensors = Some(Sensors::start());
            high.running_s = 0.0;
            high.last_line_s = 0.0;
            high.last_counts = Counts::default();
            high.logged_lens = false;
            high.logged_error = false;
            high.last_frame = None;
            high.frame_age_ms = None;
            high.gyro_peak = 0.0;
            high.accel_sum = 0.0;
            high.accel_n = 0;
            high.last_gyro_t = None;
            high.gyro_gap_ms = 0.0;
            log.note(format!(
                "on · {}",
                crate::platform::precise_sensors::device_name()
            ));
        }
        (false, true) => {
            high.sensors = None;
            log.note("off");
        }
        _ => {}
    }
    if let Some(sensors) = &high.sensors {
        high.running_s += time.delta_secs();
        if let Some(e) = sensors.error().filter(|_| !high.logged_error) {
            high.logged_error = true;
            log.note(format!("error · {e}"));
        }
        if !high.logged_lens {
            if let Some(lens) = sensors.lens() {
                high.logged_lens = true;
                log.note(format!(
                    "lens · {}",
                    serde_json::to_string(&lens).unwrap_or_default()
                ));
            }
        }
        // Until the matcher exists, frames and motion are looked at for the log
        // and dropped.
        if let Some(frame) = sensors.take_frame() {
            high.last_frame = Some((frame.width, frame.height, frame.mean()));
            let now = crate::platform::precise_sensors::now_ns();
            high.frame_age_ms = Some((now - frame.t_ns) as f64 / 1e6);
        }
        for m in sensors.drain_motion() {
            let [x, y, z] = m.xyz;
            let len = (x * x + y * y + z * z).sqrt();
            match m.kind {
                MotionKind::Gyro => {
                    high.gyro_peak = high.gyro_peak.max(len);
                    if let Some(prev) = high.last_gyro_t {
                        high.gyro_gap_ms = high.gyro_gap_ms.max((m.t_ns - prev) as f64 / 1e6);
                    }
                    high.last_gyro_t = Some(m.t_ns);
                }
                MotionKind::Accel => {
                    high.accel_sum += len;
                    high.accel_n += 1;
                }
            }
        }

        let span = high.running_s - high.last_line_s;
        if span >= LOG_EVERY_S {
            let now = sensors.counts();
            let rate = |a: u64, b: u64| (a - b) as f32 / span;
            let mut line = format!(
                "t+{:.0}s · camera {:.1}/s · accel {:.0}/s · gyro {:.0}/s",
                high.running_s,
                rate(now.frames, high.last_counts.frames),
                rate(now.accel, high.last_counts.accel),
                rate(now.gyro, high.last_counts.gyro),
            );
            if let Some((w, h, mean)) = high.last_frame {
                line.push_str(&format!(" · frame {w}×{h} brightness {mean:.0}"));
            }
            if let Some(age) = high.frame_age_ms {
                line.push_str(&format!(" · frame age {age:.0} ms"));
            }
            if high.accel_n > 0 {
                line.push_str(&format!(
                    " · gravity {:.2} m/s² · turn peak {:.2} rad/s · gyro gap {:.0} ms",
                    high.accel_sum / high.accel_n as f32,
                    high.gyro_peak,
                    high.gyro_gap_ms
                ));
            }
            high.gyro_gap_ms = 0.0;
            high.gyro_peak = 0.0;
            high.accel_sum = 0.0;
            high.accel_n = 0;
            match tracking.last_fix() {
                Some(f) => line.push_str(&format!(
                    " · gnss {:.6},{:.6} ±{:.1} m",
                    f.lat, f.lng, f.accuracy_m
                )),
                None => line.push_str(" · gnss none"),
            }
            match tracking.heading {
                Some(h) => line.push_str(&format!(
                    " · compass {:.0}°{}",
                    h.bearing_deg(),
                    h.accuracy_deg
                        .map(|a| format!(" ±{a:.0}°"))
                        .unwrap_or_default()
                )),
                None => line.push_str(" · compass none"),
            }
            log.note(line);
            high.last_counts = now;
            high.last_line_s = high.running_s;
        }
    }
    log.save();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_drops_its_oldest_lines_past_its_size() {
        let mut log = HighPrecisionLog {
            lines: VecDeque::new(),
            bytes: 0,
            dirty: false,
        };
        let line = "x".repeat(1000);
        for _ in 0..40 {
            log.push_line(line.clone());
        }
        assert!(log.bytes <= LOG_MAX_BYTES);
        assert_eq!(log.lines.len(), LOG_MAX_BYTES / 1001);
        assert_eq!(log.text().len(), log.bytes);
        log.clear();
        assert!(log.is_empty() && log.dirty);
    }
}
