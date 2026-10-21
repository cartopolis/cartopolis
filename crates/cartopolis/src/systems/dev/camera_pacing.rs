//! How evenly a drag moves the camera, frame by frame.
//!
//! A drag moves the camera by the cursor's movement since the last frame
//! (`free_camera`'s modes apply each frame's `CursorMoved` deltas as they come),
//! so what a person sees is the cursor's path sampled at frame times. It reads as
//! smooth only when two things are even: the frames themselves, and the pointer
//! events between them. A browser can deliver a frame's worth of pointer events
//! late and the next frame two frames' worth, and a camera that moves 0, then 2,
//! then 1 steps judders at a steady 60 fps. A frame-rate figure cannot see that.
//!
//! So while the left or middle button is held and the cursor moves, this logs,
//! every five seconds, a `camera pacing` line:
//!
//! * the frame intervals: median, 95th percentile and worst;
//! * how many pointer events each frame got (`ev0` … `ev3plus` frames);
//! * `speed_cv`: the spread of cursor pixels per millisecond across frames,
//!   standard deviation over mean. A drag at a constant speed (the probes'
//!   `drag` script) should read near 0; every frame that moved the camera too
//!   far or not at all for its time raises it;
//! * `off_pace`: frames whose pixels per millisecond were under half or over
//!   twice the window's median, the judders a person would count;
//! * `applied_cv` and `applied_off_pace`: the same two for what the camera was
//!   actually moved by ([`AppliedDrag`], written by Earth mode), which on the web
//!   is the pointer's timed trail replayed at an even pace
//!   (`platform::pointer_trail`). The pair against the pair above is the fix
//!   against the input it fixed.
//!
//! The browser probes collect the line (`tools/gpu_log.ts`).

use bevy::prelude::*;
use bevy::window::CursorMoved;

/// What a drag moved the camera by this frame, in logical pixels. Earth mode
/// adds to it; [`measure_camera_pacing`] reads and clears it in `Last`.
#[derive(Resource, Default)]
pub struct AppliedDrag(pub f32);

/// One frame of a drag: interval in ms, cursor pixels, events, applied pixels.
type DragFrame = (f32, f32, u32, f32);

/// Five seconds of drag frames.
#[derive(Default)]
pub struct DragWindow {
    started: Option<f64>,
    last_pos: Option<Vec2>,
    frames: Vec<DragFrame>,
}

/// `Last`: one frame's worth of pointer movement against its interval.
pub fn measure_camera_pacing(
    time: Res<Time<Real>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut cursor: MessageReader<CursorMoved>,
    mut applied: ResMut<AppliedDrag>,
    mut window: Local<DragWindow>,
) {
    let applied_px = std::mem::take(&mut applied.0);
    let dragging = mouse.pressed(MouseButton::Left) || mouse.pressed(MouseButton::Middle);
    let mut events = 0u32;
    let mut pixels = 0.0f32;
    for e in cursor.read() {
        if dragging {
            if let Some(last) = window.last_pos {
                pixels += (e.position - last).length();
            }
            events += 1;
        }
        window.last_pos = Some(e.position);
    }
    if !dragging {
        window.last_pos = None;
    }
    let now = time.elapsed_secs_f64();
    let started = *window.started.get_or_insert(now);
    // A frame of a drag that moved, and the frames between moves while the
    // button stays down: a stalled frame is exactly what this is looking for.
    if dragging && (pixels > 0.0 || !window.frames.is_empty()) {
        window
            .frames
            .push((time.delta_secs() * 1000.0, pixels, events, applied_px));
    }
    if now - started >= 5.0 {
        report(&window.frames);
        window.frames.clear();
        window.started = Some(now);
    }
}

/// Spread of pixels per millisecond (standard deviation over mean), frames off
/// pace (under half or over twice the median), and the mean.
fn pace(frames: &[DragFrame], px: impl Fn(&DragFrame) -> f32) -> (f32, usize, f32) {
    let speed: Vec<f32> = frames
        .iter()
        .map(|f| if f.0 > 0.0 { px(f) / f.0 } else { 0.0 })
        .collect();
    let mean = speed.iter().sum::<f32>() / speed.len() as f32;
    let var = speed.iter().map(|s| (s - mean).powi(2)).sum::<f32>() / speed.len() as f32;
    let mut sorted = speed.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    let off = speed
        .iter()
        .filter(|&&s| median > 0.0 && (s < 0.5 * median || s > 2.0 * median))
        .count();
    let cv = if mean > 0.0 { var.sqrt() / mean } else { 0.0 };
    (cv, off, mean)
}

fn report(frames: &[DragFrame]) {
    // A few frames is a click, not a drag.
    if frames.len() < 20 {
        return;
    }
    let mut dt: Vec<f32> = frames.iter().map(|f| f.0).collect();
    dt.sort_by(f32::total_cmp);
    let pct = |p: f32| dt[((dt.len() - 1) as f32 * p).round() as usize];
    let (cv, off_pace, mean) = pace(frames, |f| f.1);
    let (applied_cv, applied_off_pace, _) = pace(frames, |f| f.3);
    let count = |n: u32| frames.iter().filter(|f| f.2 == n).count();
    let r = |v: f32| (v * 10.0).round() / 10.0;
    tracing::info!(
        frames = frames.len(),
        dt_p50_ms = r(pct(0.5)),
        dt_p95_ms = r(pct(0.95)),
        dt_max_ms = r(pct(1.0)),
        ev0 = count(0),
        ev1 = count(1),
        ev2 = count(2),
        ev3plus = frames.iter().filter(|f| f.2 >= 3).count(),
        px_per_ms = r(mean),
        speed_cv = (cv * 100.0).round() / 100.0,
        off_pace,
        applied_cv = (applied_cv * 100.0).round() / 100.0,
        applied_off_pace,
        "camera pacing",
    );
}
