//! The clock the moving world reads, as distinct from the clock the loaders read.
//!
//! Traffic, walkers, signals, the cloud deck, wind turbines — and, through
//! `globals.time`, water waves, grass and particles — animate on [`AnimClock`].
//! In every ordinary run it *is* the virtual clock, copied each frame, so nothing
//! moves differently for it existing.
//!
//! It exists for one thing: a scripted capture rendered as a movie
//! (`--video-fps N`). There every frame first waits for the streamers to settle,
//! which takes anywhere from a fraction of a second to twenty, and the streamers'
//! own throttles and timeouts run on the virtual clock, so that clock cannot stop.
//! Had the moving world read it too, each frame would move the cars and the waves
//! by however long that frame happened to load: a movie of jumps. Here it stands
//! still while a frame settles and steps exactly `1/N` s between captures, so the
//! frames string together at real speed whatever each one cost.

use std::time::Duration;

use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use bevy::render::RenderApp;

/// What the moving world animates against. Read it as you would `Res<Time>`.
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct AnimClock {
    elapsed: f64,
    delta: f32,
    /// `Some(1/fps)` under `--video-fps`: the clock moves only by
    /// [`AnimClock::step`], once per capture.
    frame_step: Option<f32>,
    pending: f32,
}

impl AnimClock {
    /// A clock that stands still until [`AnimClock::step`] moves it a frame.
    #[must_use]
    pub fn video(fps: f32) -> Self {
        Self {
            frame_step: Some(1.0 / fps),
            ..Self::default()
        }
    }

    /// Whether this run is rendering a movie (`--video-fps`).
    #[must_use]
    pub fn is_video(&self) -> bool {
        self.frame_step.is_some()
    }

    /// Ask for one frame's worth of time on the next tick. A no-op outside a movie.
    pub fn step(&mut self) {
        if let Some(step) = self.frame_step {
            self.pending += step;
        }
    }

    #[must_use]
    pub fn delta_secs(&self) -> f32 {
        self.delta
    }

    #[must_use]
    pub fn elapsed_secs(&self) -> f32 {
        self.elapsed as f32
    }

    /// Wrapped as `globals.time` is (Bevy wraps it at an hour), for a phase a
    /// shader compares against.
    #[must_use]
    pub fn elapsed_secs_wrapped(&self) -> f32 {
        self.elapsed.rem_euclid(3600.0) as f32
    }
}

/// `First`, after Bevy's own time update: copy the virtual clock, or in a movie
/// spend whatever step was asked for.
pub fn tick_anim_clock(time: Res<Time<Virtual>>, mut clock: ResMut<AnimClock>) {
    if clock.frame_step.is_some() {
        let step = std::mem::take(&mut clock.pending);
        clock.delta = step;
        clock.elapsed += f64::from(step);
    } else {
        clock.delta = time.delta_secs();
        clock.elapsed = time.elapsed_secs_f64();
    }
}

/// Render world, between extraction and `PrepareResources`: in a movie, hand the
/// shaders the animation clock as `globals.time`, which Bevy otherwise fills from
/// the virtual clock. Outside a movie the extracted time is left alone.
pub fn shader_time_from_anim_clock(clock: Option<Res<AnimClock>>, mut time: ResMut<Time>) {
    let Some(clock) = clock else {
        return;
    };
    if !clock.is_video() {
        return;
    }
    let mut t = Time::<()>::default();
    let before = (clock.elapsed - f64::from(clock.delta)).max(0.0);
    t.advance_to(Duration::from_secs_f64(before));
    t.advance_by(Duration::from_secs_f32(clock.delta));
    *time = t;
}
