//! Where each frame's time went: the main world's update, the render world's
//! extract-to-present, and everything outside both.
//!
//! The browser has no profiler a scripted run can rely on (Firefox's needs a
//! person, Chrome's sampler starves while the page compiles), and a frame-rate
//! figure alone cannot say whether a freeze was this client's code or the
//! browser's. So the client times its own two halves and logs, every five seconds:
//!
//! * `main`: from the first system of `First` to the last of `Last`;
//! * `render`: from the render world's extract to the end of its `PostCleanup`,
//!   which includes acquiring the swap-chain texture and presenting it;
//! * `other`: the frame minus both. On the web that is the browser's own work,
//!   the `IoTaskPool` futures that run between frames (on the page's thread), and
//!   the wait for the next animation frame. A freeze that is mostly `other` is not
//!   in a system at all.
//!
//! Natively the render world runs a frame behind on its own thread, so `main` and
//! `render` overlap there rather than adding up, and `other` reads as the idle.
//! The browser probes collect the line (`tools/gpu_log.ts`).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use bevy::prelude::*;

/// Microseconds since the first call, from any thread.
fn now_us() -> u64 {
    static START: std::sync::OnceLock<bevy::platform::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(bevy::platform::time::Instant::now)
        .elapsed()
        .as_micros() as u64
}

static MAIN_START: AtomicU64 = AtomicU64::new(0);
static MAIN_US: AtomicU64 = AtomicU64::new(0);
static RENDER_START: AtomicU64 = AtomicU64::new(0);
static RENDER_US: AtomicU64 = AtomicU64::new(0);

/// `First`, before time advances: one frame ends and the next begins.
pub fn begin_main(mut window: Local<FrameWindow>) {
    let now = now_us();
    let last = MAIN_START.swap(now, Relaxed);
    if last != 0 {
        window.add(
            now.saturating_sub(last),
            MAIN_US.load(Relaxed),
            RENDER_US.load(Relaxed),
        );
    }
    if window.until_report == 0 {
        window.until_report = now + 5_000_000;
    } else if now >= window.until_report {
        window.report();
        *window = FrameWindow {
            until_report: now + 5_000_000,
            ..FrameWindow::default()
        };
    }
}

/// `Last`: the main world's share of this frame.
pub fn end_main() {
    let start = MAIN_START.load(Relaxed);
    MAIN_US.store(now_us().saturating_sub(start), Relaxed);
}

/// The render world's `ExtractSchedule`.
pub fn begin_render() {
    RENDER_START.store(now_us(), Relaxed);
    #[cfg(heap_labels)]
    crate::platform::heap::labels::set("render");
}

/// The render world's `PostCleanup`, after the frame was presented.
pub fn end_render() {
    let start = RENDER_START.load(Relaxed);
    RENDER_US.store(now_us().saturating_sub(start), Relaxed);
    #[cfg(heap_labels)]
    crate::platform::heap::labels::set("main");
}

/// Five seconds of frames.
#[derive(Default)]
pub struct FrameWindow {
    until_report: u64,
    frames: u32,
    frame_us: u64,
    main_us: u64,
    render_us: u64,
    max_main_us: u64,
    max_render_us: u64,
    /// The longest frame, and its main and render shares.
    worst: (u64, u64, u64),
}

impl FrameWindow {
    fn add(&mut self, frame: u64, main: u64, render: u64) {
        self.frames += 1;
        self.frame_us += frame;
        self.main_us += main;
        self.render_us += render;
        self.max_main_us = self.max_main_us.max(main);
        self.max_render_us = self.max_render_us.max(render);
        if frame > self.worst.0 {
            self.worst = (frame, main, render);
        }
    }

    fn report(&self) {
        if self.frames == 0 {
            return;
        }
        let n = u64::from(self.frames);
        let ms = |us: u64| (us as f64 / 100.0).round() / 10.0;
        let (wf, wm, wr) = self.worst;
        tracing::info!(
            frames = self.frames,
            mean_ms = ms(self.frame_us / n),
            main_ms = ms(self.main_us / n),
            render_ms = ms(self.render_us / n),
            other_ms = ms((self.frame_us.saturating_sub(self.main_us + self.render_us)) / n),
            max_main_ms = ms(self.max_main_us),
            max_render_ms = ms(self.max_render_us),
            worst_ms = ms(wf),
            worst_main_ms = ms(wm),
            worst_render_ms = ms(wr),
            worst_other_ms = ms(wf.saturating_sub(wm + wr)),
            "frame split",
        );
    }
}
