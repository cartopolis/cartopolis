//! The streaming ring as a **controlled variable**, not a constant.
//!
//! # What was missing
//!
//! `quality::Budget` decides how far every streamer reaches — `radius_bias`,
//! `tile_px`, `detail_layers` — once, at startup, from the class of machine. The
//! quantity those settings exist to bound is **resident GPU content**, and
//! `quality::handset_guarded`'s own note ends by saying the honest thing about
//! it: 991 MB → 371 MB is measured, *and therefore the device survives* is not.
//! The reason it cannot be said is that a radius is a proxy. A ring of a given
//! size holds whatever the ground under it happens to hold, and the ground varies
//! by more than the rungs do — the 371 MB in that table is Groningen, and nothing
//! in the client would notice a city that is denser still.
//!
//! `upload_meter::resident_bytes` measures the real quantity, and until now
//! nothing read it except a log line and a metrics field. This closes that loop:
//! the measurement moves the ring.
//!
//! # The rules, and why each one is shaped the way it is
//!
//! * **It only ever tightens the ring the tier already chose.** `radius` is
//!   `Budget::radius` minus a bias this module owns, never plus — a controller
//!   that could *widen* past the tier would be second-guessing the one decision
//!   that was made with the machine in hand.
//! * **Tighten quickly, relax slowly.** Over budget is a device the client may be
//!   about to lose; under budget is a view that could be a little richer. Those
//!   are not symmetrical stakes, so the thresholds are not symmetrical either
//!   ([`TIGHTEN_AFTER_S`] against [`RELAX_AFTER_S`]).
//! * **Relaxing has hysteresis.** It needs residency under
//!   [`RELAX_FRACTION`] of the ceiling, not merely under the ceiling, or the
//!   controller sits on the boundary retearing the ring in both directions for
//!   ever.
//! * **A dwell time, because a change is expensive.** Every streamer answers a
//!   smaller radius by retiring a ring of cells and a larger one by fetching one,
//!   and mass despawns are the documented device-loss pattern this is trying to
//!   stay away from. [`MIN_DWELL_S`] means the cure cannot become the disease.
//! * **A hard bound** ([`MAX_BIAS`]). Two steps takes the finest clipmap ring
//!   from 9×9 to 5×5; past that the map stops being a map. If two steps are not
//!   enough the answer is a smaller `tile_px` or a lower rung, which are
//!   decisions for a person.
//!
//! # It is off where a wandering radius would be wrong
//!
//! **Scripted captures.** `--shot` exists to photograph a stated viewpoint
//! reproducibly, and a ring that changed size according to how much the previous
//! shot in a sweep happened to hold would make two runs of `cargo shots` differ
//! for reasons no one could see in the image. The system is gated on
//! `shot_harness::harness_active`. This is the opposite choice from
//! `stream_ledger`, which stays in force during a capture, and the difference is
//! what each one does when it acts: a deferred upload arrives a frame later and
//! the settle test waits for it, while a narrower ring means content that is
//! never fetched at all.
//!
//! **Machines with no ceiling.** `Budget::resident_bytes_target` is `None` on the
//! desktop rungs, and [`observe`] returns immediately there — so a workstation
//! runs the code that existed before this module, byte for byte.

use crate::platform::sync::Mutex;

/// How much detail a *cell* is built with, from how far it is from the one the
/// camera is over.
///
/// # Why this is a coarse two-state answer and not a continuous one
///
/// A cell's detail is baked into its merged meshes on a worker, so changing it
/// means retearing and rebuilding the cell — which is the mass-churn pattern
/// `docs/notes/upload-bursts.md` names as the `VK_ERROR_DEVICE_LOST` cause. Every
/// extra rung is another boundary the camera can cross, and every crossing is a
/// rebuild. Two states with one boundary, placed where crossing it is rare, is the
/// most this can afford.
///
/// # And why the boundary is where it is
///
/// A ring streamer's cells are big — a z14 cell is about 1.5 km at Dutch
/// latitudes — so "one ring out" does **not** mean "far away": a rank-1 cell is
/// adjacent, and its near edge is under your feet. Only from rank 2 is the whole
/// cell at least one cell's width away, which is why [`for_rank`](Self::for_rank)
/// takes the last near rank from the caller rather than assuming one, and why
/// this is worth applying only to streamers whose ring is wide enough to have a
/// rank 2 at all. `map_geometry` (radius 1) has no such rank and does not use
/// this; `lod22` (radius 2) does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Detail {
    /// Everything the cell can carry.
    Near,
    /// The cell without the detail that cannot be resolved from a cell's width
    /// away.
    Far,
}

impl Detail {
    /// [`Near`](Self::Near) up to and including `last_near_rank`, [`Far`](Self::Far)
    /// beyond it.
    #[must_use]
    pub fn for_rank(rank: crate::platform::fetch_gate::Rank, last_near_rank: i64) -> Self {
        if i64::from(rank) <= last_near_rank {
            Self::Near
        } else {
            Self::Far
        }
    }
}

/// Steps of radius the controller may take away. Two, for the reason the module
/// note gives.
const MAX_BIAS: i64 = 2;

/// Continuous seconds over the ceiling before the ring is tightened.
const TIGHTEN_AFTER_S: f64 = 3.0;

/// Continuous seconds over the ceiling before the **retained-mesh pool** is
/// dropped — the cheap half of this controller's response, and the one it
/// reaches for first.
///
/// Deliberately shorter than [`TIGHTEN_AFTER_S`], and deliberately not subject
/// to [`MIN_DWELL_S`], because the two responses do not cost the same thing. A
/// step of radius takes away map the person is looking at and retears a ring in
/// every streamer, which is why it is slow and rationed. Dropping the pool takes
/// away nothing that is drawn: the bytes belong to cells that have already left
/// the ring, and the only cost of losing them is that a return journey rebuilds
/// what it would otherwise have re-spawned.
///
/// So the order is shed at one second, tighten at three — and the two seconds
/// between them are the shed's chance to work. If the pool was the pressure, the
/// residency falls, the run of over-ceiling samples ends on its own and the
/// tighten never arrives. If it was not, the countdown is undisturbed.
const SHED_AFTER_S: f64 = 1.0;

/// Continuous seconds comfortably under it before a step is given back.
const RELAX_AFTER_S: f64 = 30.0;

/// How far under the ceiling "comfortably" is. A step of radius is worth roughly
/// a third of the content at these ring sizes, so relaxing at 0.6 leaves room to
/// land under the ceiling rather than straight back over it.
const RELAX_FRACTION: f64 = 0.6;

/// Minimum time between changes, whichever direction. A change retears rings in
/// every streamer at once.
const MIN_DWELL_S: f64 = 15.0;

/// What the controller has decided, and the evidence it is accumulating.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Control {
    /// Steps of radius currently withheld.
    bias: i64,
    /// Clock reading of the last change, or `None` before the first one.
    changed_at: Option<f64>,
    /// When the current run of over-ceiling samples began.
    over_since: Option<f64>,
    /// When the current run of comfortably-under samples began.
    under_since: Option<f64>,
    /// Set when the retained-mesh pool should be dropped, until someone reads it.
    ///
    /// An edge rather than a level, so a streamer that reads it clears its pool
    /// once rather than on every frame the machine is over its ceiling.
    shed: bool,
    /// Whether this run of over-ceiling samples has already had its shed.
    ///
    /// Cleared the moment residency comes back under the ceiling, so a later
    /// spell of pressure gets its own. Without it the shed fires every frame
    /// after the first second, which costs nothing once the pool is empty and
    /// reads, in a log, as a controller doing something when it is not.
    shed_done: bool,
}

const IDLE: Control = Control {
    bias: 0,
    changed_at: None,
    over_since: None,
    under_since: None,
    shed: false,
    shed_done: false,
};

/// Whether a streamer holding a retained-mesh pool should drop it now.
///
/// Consuming: the flag is an edge, so the first reader clears it. There is one
/// reader today (`systems::lod22`); a second would need this to fan out rather
/// than to race, and the assertion in `a_shed_is_consumed_once` is what would
/// fail when somebody adds it.
#[must_use]
pub fn take_shed() -> bool {
    let mut control = CONTROL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::mem::replace(&mut control.shed, false)
}

static CONTROL: Mutex<Control> = Mutex::new(IDLE);

/// How far a streamer may reach, with both the tier's decision and the
/// controller's applied.
///
/// The drop-in replacement for `quality::budget().radius(base)`, and every ring
/// streamer calls it instead. Floored at 1 by `Budget::radius`, which is what
/// stops any amount of pressure reducing a streamer to nothing.
#[must_use]
pub fn radius(base: i64) -> i64 {
    let bias = CONTROL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .bias;
    // `Budget::radius` applies the tier's own bias and floors the result at 1;
    // subtracting first and flooring once is what keeps that floor the only one.
    crate::quality::budget().radius(base - bias)
}

/// Steps of radius the controller is currently withholding — 0 when it has never
/// acted. Read by the diagnostics readout and the capture metrics, so a picture
/// with a narrower ring than the tier asked for says so.
#[must_use]
pub fn bias() -> i64 {
    CONTROL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .bias
}

/// Feed the controller one frame's measurement.
///
/// Runs every frame off `upload_meter`'s running total, which costs an atomic
/// read — the meter maintains `resident_bytes` by difference from asset events,
/// so nothing here walks the scene.
pub fn observe(
    meter: bevy::prelude::Res<crate::systems::upload_meter::UploadMeter>,
    time: bevy::prelude::Res<bevy::prelude::Time>,
) {
    let Some(ceiling) = crate::quality::budget().resident_bytes_target else {
        return;
    };
    let decided = {
        let mut control = CONTROL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = control.bias;
        *control = step(
            *control,
            meter.resident_bytes,
            ceiling,
            crate::quality::warm_gpu_ceiling() > 0,
            time.elapsed_secs_f64(),
        );
        (before != control.bias).then_some((before, control.bias))
    };
    if let Some((before, after)) = decided {
        tracing::info!(
            resident_mb = meter.resident_bytes / (1024 * 1024),
            ceiling_mb = ceiling / (1024 * 1024),
            from = before,
            to = after,
            "streaming radius adjusted to the measured residency"
        );
    }
}

/// The decision itself: pure over the state, the measurement and the clock, so
/// every rule above can be asserted without a renderer, a scene or a wall clock.
///
/// `pool` says whether there is a retained-mesh pool to shed at all — passed in
/// rather than read from [`crate::quality`], because a `OnceLock` resolved from
/// the machine this happens to be running on is exactly the kind of hidden input
/// the sentence above promises this function does not have.
#[must_use]
fn step(mut control: Control, resident: usize, ceiling: usize, pool: bool, now: f64) -> Control {
    let over = resident > ceiling;
    #[allow(clippy::cast_precision_loss)]
    let comfortable = (resident as f64) < (ceiling as f64) * RELAX_FRACTION;

    // Each run of samples is timed from when it started, and any sample of the
    // other kind ends it. A single frame under the ceiling therefore resets the
    // countdown to tighten — which is right: the thing being defended against is
    // sustained pressure, and a one-frame dip means the pressure is not sustained.
    control.over_since = over.then(|| control.over_since.unwrap_or(now));
    control.under_since = comfortable.then(|| control.under_since.unwrap_or(now));
    // The pressure is off, so the next run of it gets a shed of its own.
    if control.over_since.is_none() {
        control.shed_done = false;
    }

    // Shed before tighten, and *before* the dwell gate — the dwell exists because
    // a radius change retears every streamer's ring, and dropping cells nobody is
    // drawing retears nothing.
    //
    // **Once per run of pressure, and it does not touch `over_since`.** Both
    // halves of that were wrong in the first version and the second is what the
    // tests caught: resetting the countdown meant a machine whose pressure was
    // the live ring rather than the pool re-shed an already-empty pool every
    // second for ever and never reached the tighten at all. What gives the shed
    // its chance is not a reset here but the residency actually falling — if
    // dropping the pool was enough, the next sample is under the ceiling,
    // `over_since` clears on its own and the map is never touched. If it was not
    // enough the countdown runs on undisturbed and the ring narrows on time.
    if pool
        && !control.shed_done
        && control
            .over_since
            .is_some_and(|since| now - since >= SHED_AFTER_S)
    {
        control.shed = true;
        control.shed_done = true;
    }

    if control.changed_at.is_some_and(|at| now - at < MIN_DWELL_S) {
        return control;
    }

    if control.bias < MAX_BIAS
        && control
            .over_since
            .is_some_and(|since| now - since >= TIGHTEN_AFTER_S)
    {
        return Control {
            bias: control.bias + 1,
            changed_at: Some(now),
            over_since: None,
            under_since: None,
            // Carried, never cleared: `take_shed` is the only consumer, and a
            // request dropped here would be one a streamer never heard.
            shed: control.shed,
            // A change resets `over_since`, so the run of pressure that follows
            // it is a new one and is entitled to its own shed.
            shed_done: false,
        };
    }
    if control.bias > 0
        && control
            .under_since
            .is_some_and(|since| now - since >= RELAX_AFTER_S)
    {
        return Control {
            bias: control.bias - 1,
            changed_at: Some(now),
            over_since: None,
            under_since: None,
            // Carried, never cleared: `take_shed` is the only consumer, and a
            // request dropped here would be one a streamer never heard.
            shed: control.shed,
            // A change resets `over_since`, so the run of pressure that follows
            // it is a new one and is entitled to its own shed.
            shed_done: false,
        };
    }
    control
}

#[cfg(test)]
mod detail_tests {
    use super::Detail;

    #[test]
    fn the_cell_under_the_camera_and_its_neighbours_are_near() {
        assert_eq!(Detail::for_rank(0, 1), Detail::Near);
        assert_eq!(Detail::for_rank(1, 1), Detail::Near);
        assert_eq!(Detail::for_rank(2, 1), Detail::Far);
    }

    /// A ring with no rank 2 in it has no far cells at all, so a streamer that
    /// adopted this by mistake would get its old behaviour rather than a
    /// silently thinner map.
    #[test]
    fn a_ring_of_one_is_near_throughout() {
        for rank in 0..=1 {
            assert_eq!(Detail::for_rank(rank, 1), Detail::Near);
        }
    }

    /// `fetch_gate::ANY` — a streamer with no distance to state — must not come
    /// out as near.
    #[test]
    fn an_unranked_cell_is_far() {
        assert_eq!(
            Detail::for_rank(crate::platform::fetch_gate::ANY, 1),
            Detail::Far
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        step, Control, IDLE, MAX_BIAS, MIN_DWELL_S, RELAX_AFTER_S, RELAX_FRACTION, SHED_AFTER_S,
        TIGHTEN_AFTER_S,
    };

    const CEILING: usize = 400 * 1024 * 1024;
    const OVER: usize = CEILING + 1;
    const COMFORTABLE: usize = (CEILING as f64 * (RELAX_FRACTION - 0.1)) as usize;
    /// Between the relax fraction and the ceiling: the band where nothing should
    /// happen in either direction.
    const SETTLED: usize = (CEILING as f64 * (RELAX_FRACTION + 0.1)) as usize;

    /// Feed one measurement repeatedly from `t0` to `t1`, a sample every 100 ms —
    /// roughly a frame, and enough resolution for the thresholds here.
    fn run(control: Control, resident: usize, from: f64, to: f64) -> Control {
        // `false`: no retained-mesh pool, which is every handset and is the
        // state every test written before the pool existed assumes.
        run_with(false, control, resident, from, to)
    }

    fn run_with(pool: bool, mut control: Control, resident: usize, from: f64, to: f64) -> Control {
        let mut now = from;
        while now <= to {
            control = step(control, resident, CEILING, pool, now);
            now += 0.1;
        }
        control
    }

    /// The cheap response comes first and comes sooner: a machine that holds a
    /// retained-mesh pool sheds it a second into the pressure, well before the
    /// three seconds that would take a step of radius off the map.
    #[test]
    fn pressure_sheds_the_retained_pool_before_it_narrows_the_ring() {
        let after = run_with(true, IDLE, OVER, 0.0, SHED_AFTER_S + 0.2);
        assert!(after.shed, "the pool was not shed under pressure");
        assert_eq!(
            after.bias, 0,
            "the ring was narrowed before the cheap option"
        );
    }

    /// And that is the whole of the trade: when the pool *was* the pressure,
    /// dropping it takes the residency back under the ceiling and the map is
    /// never touched. The streamer freeing its meshes is what the second reading
    /// stands for here.
    #[test]
    fn a_shed_that_relieves_the_pressure_leaves_the_map_alone() {
        let shed = run_with(true, IDLE, OVER, 0.0, SHED_AFTER_S + 0.2);
        assert!(shed.shed);
        let after = run_with(
            true,
            shed,
            SETTLED,
            SHED_AFTER_S + 0.3,
            TIGHTEN_AFTER_S + 5.0,
        );
        assert_eq!(after.bias, 0, "the ring narrowed after the shed had worked");
    }

    /// And when it was not the pressure — the live ring is — the controller
    /// still reaches the expensive answer, on the original schedule.
    ///
    /// This is the case the first version of the shed got wrong: it reset the
    /// countdown, so a machine over its ceiling for a different reason re-shed an
    /// empty pool every second and never tightened at all.
    #[test]
    fn pressure_that_outlives_the_shed_still_tightens() {
        let after = run_with(true, IDLE, OVER, 0.0, TIGHTEN_AFTER_S + 0.5);
        assert!(after.shed, "the cheap option was skipped");
        assert_eq!(after.bias, 1, "the expensive one never arrived");
    }

    /// Once per run of pressure, not once a frame. An empty pool re-shed every
    /// frame costs nothing and misreports the controller as acting.
    #[test]
    fn the_pool_is_shed_once_per_run_of_pressure() {
        let mut control = IDLE;
        let mut sheds = 0;
        let mut now = 0.0;
        while now <= TIGHTEN_AFTER_S + 5.0 {
            control = step(control, OVER, CEILING, true, now);
            // Stand in for `take_shed`, which is what a streamer does with it.
            if std::mem::replace(&mut control.shed, false) {
                sheds += 1;
            }
            now += 0.1;
        }
        // Two at most: one for the first run of pressure, and one for the run
        // that starts again after the tighten resets the countdown.
        assert!(
            (1..=2).contains(&sheds),
            "the pool was shed {sheds} times in one spell of pressure"
        );
    }

    /// A rung with no pool — every handset — behaves exactly as it did before
    /// this existed.
    #[test]
    fn a_machine_with_no_pool_never_sheds() {
        let after = run_with(false, IDLE, OVER, 0.0, TIGHTEN_AFTER_S + 0.5);
        assert!(!after.shed);
        assert_eq!(after.bias, 1);
    }

    /// A pending request survives the observations between it being made and a
    /// streamer reading it — clearing it in `step` would drop sheds on the floor
    /// whenever the reader's system happened to run after the controller's.
    #[test]
    fn a_pending_shed_is_not_cleared_by_later_observations() {
        let shed = run_with(true, IDLE, OVER, 0.0, SHED_AFTER_S + 0.2);
        assert!(shed.shed);
        let later = run_with(true, shed, SETTLED, 5.0, 8.0);
        assert!(later.shed, "the request was lost before anyone read it");
    }

    #[test]
    fn sustained_pressure_tightens_the_ring() {
        let after = run(IDLE, OVER, 0.0, TIGHTEN_AFTER_S + 0.5);
        assert_eq!(after.bias, 1, "the ring was not tightened under pressure");
    }

    #[test]
    fn a_brief_spike_does_not() {
        let after = run(IDLE, OVER, 0.0, TIGHTEN_AFTER_S - 1.0);
        assert_eq!(after.bias, 0);
    }

    /// A dip below the ceiling ends the run, so the countdown starts again. This
    /// is the difference between "over budget for three seconds" and "over budget
    /// three times in three seconds".
    #[test]
    fn a_dip_under_the_ceiling_restarts_the_countdown() {
        let mut control = run(IDLE, OVER, 0.0, TIGHTEN_AFTER_S - 0.5);
        control = step(control, SETTLED, CEILING, false, TIGHTEN_AFTER_S - 0.4);
        control = run(control, OVER, TIGHTEN_AFTER_S - 0.3, TIGHTEN_AFTER_S + 0.5);
        assert_eq!(control.bias, 0, "the countdown survived a dip");
    }

    #[test]
    fn the_bias_is_bounded() {
        let mut control = IDLE;
        let mut now = 0.0;
        // Far longer than it could ever need, so only the bound can stop it.
        for _ in 0..10 {
            control = run(
                control,
                OVER,
                now,
                now + MIN_DWELL_S + TIGHTEN_AFTER_S + 1.0,
            );
            now += MIN_DWELL_S + TIGHTEN_AFTER_S + 1.0;
        }
        assert_eq!(control.bias, MAX_BIAS);
    }

    #[test]
    fn a_quiet_scene_gives_a_step_back() {
        let tightened = run(IDLE, OVER, 0.0, TIGHTEN_AFTER_S + 0.5);
        assert_eq!(tightened.bias, 1);
        let start = TIGHTEN_AFTER_S + 0.6;
        let relaxed = run(
            tightened,
            COMFORTABLE,
            start,
            start + MIN_DWELL_S + RELAX_AFTER_S + 1.0,
        );
        assert_eq!(relaxed.bias, 0, "a quiet scene never got its ring back");
    }

    /// Merely under the ceiling is not enough to relax — that is the hysteresis,
    /// and without it the controller oscillates on the boundary for ever,
    /// retearing every streamer's ring each time.
    #[test]
    fn sitting_just_under_the_ceiling_does_not_relax() {
        let tightened = run(IDLE, OVER, 0.0, TIGHTEN_AFTER_S + 0.5);
        let start = TIGHTEN_AFTER_S + 0.6;
        let after = run(
            tightened,
            SETTLED,
            start,
            start + MIN_DWELL_S + RELAX_AFTER_S * 3.0,
        );
        assert_eq!(after.bias, 1, "the controller relaxed on the boundary");
    }

    /// Two changes cannot land inside one dwell, whichever direction they are.
    #[test]
    fn a_change_is_followed_by_a_dwell() {
        let tightened = run(IDLE, OVER, 0.0, TIGHTEN_AFTER_S + 0.5);
        let at = tightened.changed_at.expect("it changed");
        let straight_after = run(tightened, OVER, at, at + MIN_DWELL_S - 1.0);
        assert_eq!(
            straight_after.bias, 1,
            "a second change landed inside the dwell"
        );
    }
}
