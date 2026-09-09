//! What this device has *proved* it can afford, as opposed to what its name says.
//!
//! **The problem this exists for.** `quality::Budget` is a judgement about a class
//! of machine, and it has to be: it is resolved in `quality::init` before a
//! renderer exists, because `platform::workers` is sized from it. That timing is
//! not negotiable, and it means the static rung can never be more than a guess.
//!
//! Worse, the guess is currently made from the operating system —
//! `auto()` is `#[cfg(target_os = "android")]` — which is precisely the
//! substitution `quality::Capabilities` documents as having been learned wrong
//! twice: "A `cfg!(target_os = "android")` meant 'a Fairphone 4' until a OnePlus 15
//! arrived and inherited a 2021 mid-ranger's sky; the fix for *that* was a table of
//! GPU families, which is the same substitution one level down." A capable handset
//! and a struggling one get the same answer, and the answer is a guess about
//! hardware nobody in the loop owns.
//!
//! **And no static capability can fix it.** `Capabilities` can ask the adapter
//! whether it filters 32-bit float textures, because that is a yes/no the device
//! reports. It cannot ask how fast the device is. wgpu exposes features and limits,
//! not throughput — and the most expensive thing this client draws is a shader
//! whose cost is ALU and 3-D noise lookups, exactly the axis on which two adapters
//! with identical feature sets differ most.
//!
//! So the honest input is not a name or a feature bit: it is **this device, drawing
//! this scene, missing or making its frame budget**. That is what this module
//! measures, and it is the only capability signal here that cannot be wrong about a
//! part nobody has tested.
//!
//! ## What it gives up first, and why that is not arbitrary
//!
//! The order is measured, not guessed. On a Steam Deck (AMD Van Gogh, RADV,
//! 2026-09-09) the globe cloud shell is the entire transparent pass: at 1280×720 it
//! is 9.4 ms of a 13.2 ms GPU frame — **71 %** — and removing it takes the frame
//! from 52 fps to 85. It is fill-bound at exactly 1.00× overdraw and scales with
//! pixel count, so a handset pays it over several times the pixels.
//! `docs/notes/render-performance-survey.md` has the table.
//!
//! Nothing else measured comes close, which is why step 1 is the clouds and there
//! is no step that drops something cheaper first.
//!
//! ## Why it is slow to give up and slower to take back
//!
//! A governor that reacts to a single slow frame is a governor that flickers, and
//! a visual effect appearing and vanishing is far worse than one that is simply
//! off. So demotion needs a *sustained* miss, promotion needs a longer and
//! *stricter* margin than demotion required, and the two thresholds deliberately do
//! not meet — the gap between them is the hysteresis band, and a device sitting in
//! it stays where it is.

use bevy::prelude::*;

use crate::resources::FrameStats;

/// How long the frame must stay over budget before an effect is given up.
///
/// Seconds of *sustained* miss, not a count of bad frames: a teleport, a pipeline
/// compile or a streaming burst all produce a cluster of slow frames that says
/// nothing about what the device can sustain, and every one of them is shorter
/// than this.
const DEMOTE_AFTER_S: f32 = 4.0;

/// How long the frame must stay comfortably *under* budget before an effect is
/// taken back. Deliberately several times [`DEMOTE_AFTER_S`]: the cost of being
/// wrong in this direction is a visible effect flickering back out again.
const PROMOTE_AFTER_S: f32 = 20.0;

/// The frame must be this far inside budget to count toward promotion.
///
/// Restoring the clouds costs ~70 % of the GPU frame at the resolution they were
/// measured at, so a device merely *scraping* its budget cannot afford them back.
/// It has to be running with room to spare, and this is that room.
const PROMOTE_HEADROOM: f32 = 0.70;

/// The most that can be given up. One step today — the clouds — because that is
/// the only effect measured to be worth a step.
pub const MAX_STEP: u8 = 1;

/// Which effects this device has been asked to give up.
///
/// `0` is everything on. Higher steps drop progressively more, cheapest-looking
/// first — except that "cheapest-looking" is decided by measurement, not by taste.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct AdaptiveQuality {
    step: u8,
    /// Seconds the frame has been continuously over budget, and continuously
    /// inside the promotion margin. Only one is ever non-zero.
    over_s: f32,
    under_s: f32,
}

impl AdaptiveQuality {
    /// Whether the globe cloud shell may be drawn.
    ///
    /// The single most expensive thing this client draws, so it is what step 1
    /// gives up. See the module doc for the measurement.
    #[must_use]
    pub fn globe_clouds(&self) -> bool {
        self.step < 1
    }

    /// How many steps have been given up, for the diagnostics readout.
    #[must_use]
    pub fn step(&self) -> u8 {
        self.step
    }
}

/// The decision, as a pure function of the accumulated timers.
///
/// Split out from the system so the hysteresis can be tested without a frame
/// loop — the thing that actually goes wrong here is oscillation, and that is a
/// property of these thresholds rather than of any one frame.
///
/// Returns the new `(step, over_s, under_s)`.
#[must_use]
pub fn decide(
    step: u8,
    over_s: f32,
    under_s: f32,
    frame_ms: f32,
    target_ms: f32,
    dt: f32,
) -> (u8, f32, f32) {
    // Over budget: accumulate toward giving something up, and forget any progress
    // toward taking something back. The two timers are exclusive on purpose — a
    // device alternating either side of the line is not "sometimes fast", it is
    // marginal, and marginal should stay where it is.
    if frame_ms > target_ms {
        let over = over_s + dt;
        if over >= DEMOTE_AFTER_S && step < MAX_STEP {
            return (step + 1, 0.0, 0.0);
        }
        return (step, over, 0.0);
    }
    // Comfortably inside budget, and only then.
    if frame_ms < target_ms * PROMOTE_HEADROOM {
        let under = under_s + dt;
        if under >= PROMOTE_AFTER_S && step > 0 {
            return (step - 1, 0.0, 0.0);
        }
        return (step, 0.0, under);
    }
    // In the band between the two thresholds: hold, and let both timers decay so a
    // long spell of "fine" does not bank credit toward a promotion it never
    // actually earned.
    (step, 0.0, 0.0)
}

/// Watch the measured frame and move the step.
///
/// Reads `FrameStats`, which `frame_pacing::frame_stats` accumulates every frame
/// on every platform whether or not anything displays it — so this costs a
/// comparison, not a measurement.
pub fn drive_adaptive_quality(
    time: Res<Time<Real>>,
    frame: Res<FrameStats>,
    mut adaptive: ResMut<AdaptiveQuality>,
) {
    // `sampled` is false until a full second has been accumulated. Acting before
    // then would demote on the startup frames, which are the slowest a run ever
    // sees and the least representative.
    if !frame.sampled {
        return;
    }
    let target_fps = crate::quality::budget().target_fps();
    if target_fps <= 0.0 {
        // No cap asked for — the run is measuring headroom (`CARTO_FPS_CAP=0`), and
        // a governor underneath that measurement would be measuring itself.
        return;
    }
    let target_ms = 1_000.0 / target_fps;
    let (step, over_s, under_s) = decide(
        adaptive.step,
        adaptive.over_s,
        adaptive.under_s,
        frame.mean_ms,
        target_ms,
        time.delta_secs(),
    );
    if step != adaptive.step {
        tracing::info!(
            from = adaptive.step,
            to = step,
            frame_ms = format!("{:.1}", frame.mean_ms),
            target_ms = format!("{:.1}", target_ms),
            "adaptive quality: this device is being asked to give up an effect \
             (step 1 = the globe cloud shell)"
        );
    }
    adaptive.step = step;
    adaptive.over_s = over_s;
    adaptive.under_s = under_s;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 60 fps: 16.67 ms.
    const TARGET: f32 = 16.67;

    /// One slow frame is not a verdict. A teleport, a pipeline compile and a
    /// streaming burst all produce clusters of them.
    #[test]
    fn a_brief_stall_does_not_cost_an_effect() {
        let (mut step, mut over, mut under) = (0u8, 0.0, 0.0);
        // Two seconds of badly missed frames — well short of the threshold.
        for _ in 0..20 {
            (step, over, under) = decide(step, over, under, 40.0, TARGET, 0.1);
        }
        assert_eq!(step, 0, "demoted on a stall shorter than DEMOTE_AFTER_S");
    }

    #[test]
    fn a_sustained_miss_gives_up_the_clouds() {
        let (mut step, mut over, mut under) = (0u8, 0.0, 0.0);
        for _ in 0..((DEMOTE_AFTER_S / 0.1) as usize + 2) {
            (step, over, under) = decide(step, over, under, 40.0, TARGET, 0.1);
        }
        assert_eq!(step, 1);
        let a = AdaptiveQuality {
            step,
            over_s: over,
            under_s: under,
        };
        assert!(!a.globe_clouds(), "step 1 must drop the globe cloud shell");
    }

    /// **The failure this module exists to avoid.** A device sitting near its
    /// budget must not toggle a visible effect on and off; an effect that flickers
    /// is worse than one that is simply absent.
    #[test]
    fn a_device_sitting_on_the_line_does_not_oscillate() {
        let (mut step, mut over, mut under) = (0u8, 0.0, 0.0);
        let mut changes = 0;
        let mut previous = step;
        // Alternate either side of the target for a simulated two minutes.
        for i in 0..1200 {
            let frame_ms = if i % 2 == 0 {
                TARGET + 2.0
            } else {
                TARGET - 2.0
            };
            (step, over, under) = decide(step, over, under, frame_ms, TARGET, 0.1);
            if step != previous {
                changes += 1;
                previous = step;
            }
        }
        assert_eq!(
            changes, 0,
            "a marginal device toggled the clouds {changes} times"
        );
    }

    /// Scraping the budget is not enough to earn an effect back that costs ~70 %
    /// of the GPU frame.
    #[test]
    fn scraping_the_budget_does_not_earn_the_clouds_back() {
        // Start demoted.
        let (mut step, mut over, mut under) = (1u8, 0.0, 0.0);
        // Just inside budget, but not inside the promotion margin, for a long time.
        for _ in 0..2000 {
            (step, over, under) = decide(step, over, under, TARGET * 0.95, TARGET, 0.1);
        }
        assert_eq!(step, 1, "promoted on a margin it had not earned");
    }

    #[test]
    fn real_headroom_takes_the_clouds_back() {
        let (mut step, mut over, mut under) = (1u8, 0.0, 0.0);
        for _ in 0..((PROMOTE_AFTER_S / 0.1) as usize + 2) {
            (step, over, under) = decide(step, over, under, TARGET * 0.4, TARGET, 0.1);
        }
        assert_eq!(step, 0);
    }

    /// Promotion must be the slower direction, or a device that cannot hold the
    /// effect spends its life re-acquiring it.
    #[test]
    fn taking_back_is_slower_than_giving_up() {
        assert!(
            PROMOTE_AFTER_S > DEMOTE_AFTER_S * 2.0,
            "promotion must be substantially slower than demotion"
        );
        assert!(
            PROMOTE_HEADROOM < 1.0,
            "promotion must require real headroom, not merely meeting the target"
        );
    }
}
