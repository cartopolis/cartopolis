//! The street crowd notices people: a walker makes way round an avatar,
//! sometimes looks at one it passes, and now and then waves back at one that
//! waves (`docs/notes/ambient-traffic.md`, "The crowd notices you").
//!
//! The walkers are `systems::traffic`'s, drawn by `systems::crowd`; this file
//! holds the decisions, as pure functions over a walker's path and the people
//! near it, and the per-walker state they carry ([`Reaction`], a field of the
//! traffic's `Car`). `drive_traffic` calls them while it moves each walker, so
//! there is no second pass over the pool and no allocation per walker: the
//! people are one short list a frame ([`CrowdPeople`]), as the crows' threats
//! are.
//!
//! Purely per viewer: the crowd is invented on each client and nobody else
//! sees the same walker, so none of this is on the wire.

use crate::utils::math::smoothstep;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::{Avatar, RemoteAvatar};
use crate::grid::anchored_xz;

// ── Making way ───────────────────────────────────────────────────────────────

/// A standing avatar's reach from its centre, metres: shoulders and elbows.
pub const AVATAR_RADIUS_M: f32 = 0.35;
/// A walker's, the same way.
pub const WALKER_RADIUS_M: f32 = 0.3;
/// The closest a walker's centre ever comes to an avatar's: the two touching.
pub const MIN_GAP_M: f32 = AVATAR_RADIUS_M + WALKER_RADIUS_M;
/// The berth a walker plans to pass at, centre to centre: a little more than
/// touching, as people give a stranger.
pub const BERTH_M: f32 = 1.0;
/// How far ahead a walker starts stepping aside, metres: about four seconds
/// at its pace, which is when somebody on a pavement adjusts.
pub const LOOKAHEAD_M: f32 = 6.0;
/// How far past it a walker keeps its berth before easing back.
pub const BEHIND_M: f32 = 2.5;
/// The furthest a walker steps from its own line.
pub const MAX_SHIFT_M: f32 = 1.4;
/// How fast a walker closes on the line it means to take, per second of
/// the gap, and the most sideways speed it takes doing so (m/s).
const SHIFT_GAIN: f32 = 1.6;
const SHIFT_RATE: f32 = 0.8;

/// The side of `travel` that `traffic::right_of` calls right.
fn right_of(travel: Vec2) -> Vec2 {
    Vec2::new(-travel.y, travel.x)
}

/// Where a walker means to be, metres right of its path (the traffic's
/// `right_of`), and how much of its pace it keeps, given the people near it.
///
/// `p` is where its path puts it this frame, `travel` the path's unit
/// direction, `shift` where it is now. For each person ahead within
/// [`LOOKAHEAD_M`] (or just behind) whose line would pass within [`BERTH_M`],
/// it picks the side of them nearer its own present shift — so a walker
/// already stepping one way keeps going that way when the person moves — and
/// eases in with distance. The largest asked for wins. A walker that is still
/// on a collision course close in slows, to a stop at touching distance.
#[must_use]
pub fn make_way(p: Vec2, travel: Vec2, shift: f32, people: &[Vec2]) -> (f32, f32) {
    make_way_ahead(p, travel, shift, people, LOOKAHEAD_M)
}

/// [`make_way`] for something that looks `lookahead` metres ahead rather
/// than a walker's [`LOOKAHEAD_M`]: a fatbike weaving through the people on
/// a pavement (`traffic`) has to start its swerve three times as far out.
#[must_use]
pub fn make_way_ahead(
    p: Vec2,
    travel: Vec2,
    shift: f32,
    people: &[Vec2],
    lookahead: f32,
) -> (f32, f32) {
    let right = right_of(travel);
    let mut want = 0.0_f32;
    let mut keep = 1.0_f32;
    for &o in people {
        let d = o - p;
        let ahead = d.dot(travel);
        if !(-BEHIND_M..=lookahead).contains(&ahead) {
            continue;
        }
        let side = d.dot(right);
        if side.abs() >= BERTH_M && (side - shift).abs() >= BERTH_M {
            continue;
        }
        // Either side of them; the one nearer where the walker already is.
        let left_of_them = (side - BERTH_M).max(-MAX_SHIFT_M);
        let right_of_them = (side + BERTH_M).min(MAX_SHIFT_M);
        let target = if (left_of_them - shift).abs() <= (right_of_them - shift).abs() {
            left_of_them
        } else {
            right_of_them
        };
        // Full from a third of the lookahead (two metres, for a walker) to
        // 0.8 m past, eased either side.
        let ease_in = 1.0 - smoothstep(lookahead / 3.0, lookahead, ahead);
        let ease_out = 1.0 - smoothstep(0.8, BEHIND_M, -ahead);
        let w = ease_in.min(ease_out);
        if (target * w).abs() > want.abs() {
            want = target * w;
        }
        // Still on course for them, close in front: slow, and stop short.
        let miss = (side - shift).abs();
        if ahead > 0.0 && miss < BERTH_M {
            let room = ((ahead - MIN_GAP_M) / 2.5).clamp(0.0, 1.0);
            let slack = ((miss - MIN_GAP_M) / (BERTH_M - MIN_GAP_M)).clamp(0.0, 1.0);
            keep = keep.min(room.max(slack).max(0.0)).min(1.0);
            if miss >= MIN_GAP_M {
                // Passing close but clear: only slow a little.
                keep = keep.max(0.6);
            }
        }
    }
    (want, keep)
}

/// Move `shift` toward `want` over `dt`: in proportion to the gap, at most
/// [`SHIFT_RATE`] sideways. Returns the new shift and the sideways speed.
#[must_use]
pub fn ease_shift(shift: f32, want: f32, dt: f32) -> (f32, f32) {
    ease_shift_at(shift, want, dt, SHIFT_RATE)
}

/// [`ease_shift`] at most `rate` m/s sideways, for something quicker than
/// a walker to swerve.
#[must_use]
pub fn ease_shift_at(shift: f32, want: f32, dt: f32, rate: f32) -> (f32, f32) {
    let v = ((want - shift) * SHIFT_GAIN * (rate / SHIFT_RATE)).clamp(-rate, rate);
    let next = shift + v * dt;
    // Never step past what was asked.
    let next = if (want - shift) * (want - next) < 0.0 {
        want
    } else {
        next
    };
    (next, v)
}

/// The last word: a walker at `p` + right × `shift` is never nearer a person
/// than [`MIN_GAP_M`]. Whoever is inside it is stepped round sideways, on the
/// side the walker is already on — the plan in [`make_way`] keeps this from
/// being needed for a standing avatar; it is here for one that walks into a
/// walker, and for a walker placed beside one.
#[must_use]
pub fn keep_clear(p: Vec2, travel: Vec2, shift: f32, people: &[Vec2]) -> f32 {
    let right = right_of(travel);
    let mut shift = shift;
    for &o in people {
        let d = o - p;
        let ahead = d.dot(travel);
        if ahead.abs() >= MIN_GAP_M {
            continue;
        }
        let side = d.dot(right);
        let need = (MIN_GAP_M * MIN_GAP_M - ahead * ahead).sqrt();
        if (side - shift).abs() >= need {
            continue;
        }
        // Stay on the side it is on; dead centre, the side nearer its line.
        let away = if shift < side || (shift == side && side > 0.0) {
            -1.0
        } else {
            1.0
        };
        shift = side + away * (need + 1e-3);
    }
    shift
}

// ── Looking ──────────────────────────────────────────────────────────────────

/// A walker passing nearer than this may look at you.
pub const LOOK_RANGE_M: f32 = 4.5;
/// How many passers-by look. Most people on a pavement do not stare.
pub const LOOK_CHANCE: f32 = 0.4;
/// The furthest a head turns from straight ahead, radians (65°); somebody
/// further round than that is let go rather than followed.
pub const HEAD_LIMIT: f32 = 1.134;
/// How fast a head turns, rad/s.
const HEAD_RATE: f32 = 2.6;

/// Whether this walker looks at the person it is meeting, and for how long:
/// [`LOOK_CHANCE`] of encounters, 1–2.5 s. Deterministic in the walker and
/// the count of its encounters, so a capture is repeatable.
#[must_use]
pub fn glance(walker: u64, encounter: u32) -> Option<f32> {
    let r = |stream: u64| {
        crate::systems::traffic::rand01(0x10_0C, walker, u64::from(encounter) * 8 + stream)
    };
    (r(0) < LOOK_CHANCE).then(|| 1.0 + r(1) * 1.5)
}

/// The head turn, radians toward the body's +x, that looks from a walker
/// facing `facing` at something `d` away (both world XZ); `None` if it is
/// further round than [`HEAD_LIMIT`] allows.
///
/// The body's +x is world (`facing.y`, −`facing.x`): the walker's transform
/// turns its +z nose onto `facing` by `atan2(facing.x, facing.y)` about +y.
#[must_use]
pub fn head_toward(facing: Vec2, d: Vec2) -> Option<f32> {
    let x = d.dot(Vec2::new(facing.y, -facing.x));
    let z = d.dot(facing);
    let yaw = x.atan2(z);
    (yaw.abs() <= HEAD_LIMIT).then_some(yaw)
}

/// Turn a head from `now` toward `want` at [`HEAD_RATE`].
#[must_use]
pub fn turn_head(now: f32, want: f32, dt: f32) -> f32 {
    now + (want - now).clamp(-HEAD_RATE * dt, HEAD_RATE * dt)
}

// ── Waving back ──────────────────────────────────────────────────────────────

/// A walker this near somebody waving may wave back.
pub const WAVE_REACH_M: f32 = 10.0;
/// …if they are within this much of straight ahead of it (radians, 100°):
/// a walker does not wave at a wave it cannot see.
pub const WAVE_SEES: f32 = 1.745;
/// The most walkers that answer one wave.
pub const MAX_WAVE_BACKS: usize = 2;
/// Seconds to blend from standing into the wave, and out of it.
pub const WAVE_BLEND_IN: f32 = 0.35;
pub const WAVE_BLEND_OUT: f32 = 0.45;
/// A walker that has not managed to stop and face the waver in this long
/// gives up and walks on.
const WAVE_GIVE_UP_S: f32 = 3.5;

/// A walker answering a wave.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveBack {
    /// Where the waver stood, world XZ.
    pub at: Vec2,
    /// Seconds since the wave was seen.
    pub t: f32,
    /// The human pause before it reacts, seconds.
    pub delay: f32,
    /// Seconds it has been stood and facing the waver; the wave plays from
    /// the moment it is.
    pub waving: Option<f32>,
}

/// What a walker answering a wave is doing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WaveStep {
    /// Has not reacted yet: walks on as it was.
    Noticing,
    /// Slowing to a stop and turning to face the waver.
    Stopping,
    /// Waving: `progress` through the clip, `blend` from standing into it.
    Waving { progress: f32, blend: f32 },
    /// Done (or gave up): walks on.
    Done,
}

impl WaveBack {
    /// Where it has got to, for a wave lasting `secs`.
    #[must_use]
    pub fn step(&self, secs: f32) -> WaveStep {
        if self.t < self.delay {
            return WaveStep::Noticing;
        }
        let Some(w) = self.waving else {
            return if self.t - self.delay > WAVE_GIVE_UP_S {
                WaveStep::Done
            } else {
                WaveStep::Stopping
            };
        };
        // Into the clip as it blends in; held on its last pose while it
        // blends out.
        let total = WAVE_BLEND_IN + secs + WAVE_BLEND_OUT;
        if secs <= 0.0 || w >= total {
            return WaveStep::Done;
        }
        let progress = ((w - WAVE_BLEND_IN * 0.5) / secs).clamp(0.0, 1.0);
        let blend = if w < WAVE_BLEND_IN {
            w / WAVE_BLEND_IN
        } else if w > WAVE_BLEND_IN + secs {
            1.0 - (w - WAVE_BLEND_IN - secs) / WAVE_BLEND_OUT
        } else {
            1.0
        };
        WaveStep::Waving {
            progress,
            blend: blend.clamp(0.0, 1.0),
        }
    }
}

/// How eager a walker at `p`, facing `facing`, is to answer a wave at `w`:
/// `None` if it is out of reach or cannot see it; otherwise lower is keener
/// (nearer, more in front).
#[must_use]
pub fn wave_candidate(p: Vec2, facing: Vec2, w: Vec2) -> Option<f32> {
    let d = w - p;
    let dist = d.length();
    if dist > WAVE_REACH_M || dist < 0.3 {
        return None;
    }
    let angle = facing.angle_to(d).abs();
    (angle <= WAVE_SEES).then(|| dist * (1.0 + angle))
}

/// The walkers, of `candidates` (pool index and [`wave_candidate`] score),
/// that answer wave number `wave`: the keenest one, and a second keenest a
/// third of the time; and each one's pause. Deterministic in the wave.
#[must_use]
pub fn pick_wave_backs(
    candidates: impl Iterator<Item = (usize, f32)>,
    wave: u32,
) -> [Option<(usize, f32)>; MAX_WAVE_BACKS] {
    let mut best: [Option<(usize, f32)>; MAX_WAVE_BACKS] = [None; MAX_WAVE_BACKS];
    for (i, score) in candidates {
        if best[0].is_none_or(|(_, s)| score < s) {
            best[1] = best[0];
            best[0] = Some((i, score));
        } else if best[1].is_none_or(|(_, s)| score < s) {
            best[1] = Some((i, score));
        }
    }
    let r = |stream: u64| crate::systems::traffic::rand01(0x0A7E, u64::from(wave), stream);
    if r(0) >= 1.0 / 3.0 {
        best[1] = None;
    }
    // A pause somebody takes to register a wave and decide to answer it.
    std::array::from_fn(|k| {
        best[k].map(|(i, _)| (i, 0.45 + r(1 + k as u64) * 0.7 + k as f32 * 0.3))
    })
}

// ── Per walker ───────────────────────────────────────────────────────────────

/// What one walker is doing about the people near it. A field of the
/// traffic's `Car`; reset when the walker is recycled.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reaction {
    /// Metres right of its path it has stepped.
    pub shift: f32,
    /// Its sideways speed, m/s, for the facing.
    pub shift_speed: f32,
    /// Its head's turn, radians toward its +x.
    pub head: f32,
    /// Seconds of looking left; 0 when it is not.
    pub glance: f32,
    /// Somebody was within [`LOOK_RANGE_M`] last frame: an encounter is on.
    pub meeting: bool,
    /// Encounters so far, the seed of whether it looks.
    pub encounters: u32,
    /// Answering a wave.
    pub wave: Option<WaveBack>,
}

impl Reaction {
    /// Forget everything: the walker has been placed somewhere else.
    pub fn reset(&mut self) {
        *self = Self {
            encounters: self.encounters,
            ..Self::default()
        };
    }

    /// Look, or stop looking: called once a frame with the walker's
    /// position, its drawn facing and the people near it.
    pub fn look(&mut self, walker: u64, p: Vec2, facing: Vec2, people: &[Vec2], dt: f32) {
        let nearest = people
            .iter()
            .map(|&o| (o, o.distance_squared(p)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, d2)| *d2 < LOOK_RANGE_M * LOOK_RANGE_M);
        let want = match (self.wave, nearest) {
            // Answering a wave: look at the waver.
            (Some(w), _) => head_toward(facing, w.at - p),
            (None, Some((o, _))) => {
                if !self.meeting {
                    self.meeting = true;
                    self.encounters = self.encounters.wrapping_add(1);
                    // Only somebody it sees coming: not from behind.
                    let ahead = (o - p).dot(facing) > -0.5;
                    self.glance = if ahead {
                        glance(walker, self.encounters).unwrap_or(0.0)
                    } else {
                        0.0
                    };
                }
                self.glance = (self.glance - dt).max(0.0);
                if self.glance > 0.0 {
                    head_toward(facing, o - p)
                } else {
                    None
                }
            }
            (None, None) => {
                self.meeting = false;
                self.glance = 0.0;
                None
            }
        };
        // Past the limit, the look is over: the head comes back.
        if want.is_none() && self.wave.is_none() {
            self.glance = 0.0;
        }
        self.head = turn_head(self.head, want.unwrap_or(0.0), dt);
    }
}

/// The people the crowd notices this frame — every avatar, the local one and
/// the others drawn — and who among them started waving.
#[derive(Resource, Default)]
pub struct CrowdPeople {
    /// Where each avatar stands, world XZ.
    pub at: Vec<Vec2>,
    /// Which way each faces, unit world XZ, beside `at` — for the cars that
    /// stop for somebody waiting at a zebra (`systems::give_way`).
    pub facing: Vec<Vec2>,
    /// Where each avatar that began a wave this frame stands.
    pub waves: Vec<Vec2>,
    /// The avatars waving last frame, to see a wave begin.
    waving: Vec<Entity>,
    /// Waves seen so far: the seed of who answers.
    pub waves_seen: u32,
}

/// What [`gather_people`] reads.
#[derive(SystemParam)]
pub struct AvatarBodies<'w, 's> {
    local: Query<
        'w,
        's,
        (
            Entity,
            &'static GridCell,
            &'static Transform,
            Option<&'static crate::systems::avatar::EmotePlayback>,
        ),
        With<Avatar>,
    >,
    remote: Query<
        'w,
        's,
        (
            Entity,
            &'static GridCell,
            &'static Transform,
            Option<&'static crate::systems::avatar::EmotePlayback>,
        ),
        (With<RemoteAvatar>, Without<Avatar>),
    >,
}

/// Fill [`CrowdPeople`] for the frame: no allocation once the lists have
/// grown to the number of avatars about.
pub fn gather_people(mut people: ResMut<CrowdPeople>, bodies: AvatarBodies) {
    let CrowdPeople {
        at,
        facing,
        waves,
        waving,
        waves_seen,
    } = &mut *people;
    at.clear();
    facing.clear();
    waves.clear();
    let mut seen = 0;
    for (entity, cell, tf, emote) in bodies.local.iter().chain(bodies.remote.iter()) {
        let p = anchored_xz(cell, tf.translation);
        at.push(p);
        // The models face +z (`avatar::facing`).
        let f = tf.rotation * Vec3::Z;
        facing.push(Vec2::new(f.x, f.z).normalize_or(Vec2::Y));
        let is_waving = emote.is_some_and(|e| e.emote() == cartopolis_core::Emote::Wave);
        if !is_waving {
            continue;
        }
        // A wave begins on the first frame it is seen; `waving` keeps the
        // ones already answered, in the order met, first `seen` entries.
        match waving.iter().position(|&e| e == entity) {
            Some(k) => waving.swap(k, seen),
            None => {
                waving.push(entity);
                let last = waving.len() - 1;
                waving.swap(last, seen);
                waves.push(p);
                *waves_seen = waves_seen.wrapping_add(1);
            }
        }
        seen += 1;
    }
    waving.truncate(seen);
}

/// `CARTO_AVATAR_WAVE=<seconds>`: the avatar waves every that many seconds of
/// the moving world's clock — the hook a capture uses to film the crowd
/// waving back, since a scripted run has nobody to press the key.
pub fn scripted_wave(
    time: Res<crate::systems::anim_clock::AnimClock>,
    mut avatar: ResMut<crate::resources::AvatarState>,
    mut every: Local<Option<Option<f32>>>,
    mut next: Local<f32>,
) {
    let every = *every.get_or_insert_with(|| {
        crate::debug_switches::env_value("CARTO_AVATAR_WAVE")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|s| *s > 0.5)
    });
    let Some(every) = every else {
        return;
    };
    let now = time.elapsed_secs();
    if *next == 0.0 {
        *next = every;
    }
    if now >= *next {
        *next = now + every;
        avatar.pending_emote = Some(cartopolis_core::Emote::Wave);
        info!("CARTO_AVATAR_WAVE: the avatar waves at {now:.1} s");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk a walker down +z past a person standing `side` metres to its
    /// right, as `drive_traffic` does: plan, ease, slow, keep clear. Returns
    /// the closest it came, the largest one-frame shove `keep_clear` had to
    /// give, its shift at the end, its widest shift, and where it ended.
    fn walk_past(side: f32, person_speed: f32) -> (f32, f32, f32, f32, f32) {
        let dt = 1.0 / 60.0;
        let travel = Vec2::Y;
        let right = right_of(travel);
        let mut z = -12.0;
        let mut shift = 0.0;
        let mut speed = 1.25;
        let mut person = right * side;
        let (mut closest, mut shove, mut widest) = (f32::MAX, 0.0_f32, 0.0_f32);
        for _ in 0..(40.0 / dt) as usize {
            person.y -= person_speed * dt;
            let people = [person];
            let p = travel * z;
            let (want, keep) = make_way(p, travel, shift, &people);
            let target = 1.25 * keep;
            speed += (target - speed).clamp(-1.4 * dt, 1.4 * dt);
            z += speed * dt;
            let p = travel * z;
            (shift, _) = ease_shift(shift, want, dt);
            let cleared = keep_clear(p, travel, shift, &people);
            shove = shove.max((cleared - shift).abs());
            shift = cleared;
            widest = widest.max(shift.abs());
            closest = closest.min((p + right * shift).distance(person));
        }
        (closest, shove, shift, widest, z)
    }

    #[test]
    fn a_walker_steps_round_a_standing_avatar_and_back() {
        for k in -10..=10 {
            let side = k as f32 * 0.1;
            let (closest, shove, end, widest, z) = walk_past(side, 0.0);
            assert!(
                closest >= MIN_GAP_M - 1e-3,
                "side {side}: came within {closest:.2} m"
            );
            assert!(
                closest >= BERTH_M * 0.85 || side.abs() >= BERTH_M,
                "side {side}: passed at {closest:.2} m, closer than it planned"
            );
            assert!(
                shove < 0.01,
                "side {side}: needed a {shove:.2} m shove — the plan was too late"
            );
            assert!(
                widest <= MAX_SHIFT_M + 1e-3,
                "side {side}: {widest:.2} m out"
            );
            assert!(
                end.abs() < 0.02,
                "side {side}: still {end:.2} m off its line"
            );
            assert!(z > 10.0, "side {side}: stopped at {z:.1}");
        }
    }

    /// Somebody walking straight at a walker: it may be shoved, but is never
    /// walked through.
    #[test]
    fn an_avatar_walking_into_a_walker_never_passes_through_it() {
        for k in -5..=5 {
            let side = k as f32 * 0.1;
            let (closest, _, end, _, _) = walk_past(side, 1.4);
            assert!(closest >= MIN_GAP_M - 1e-3, "side {side}: {closest:.2} m");
            assert!(end.abs() < 0.02, "side {side}: still {end:.2} m off");
        }
    }

    #[test]
    fn nobody_near_means_no_step_and_full_pace() {
        let (want, keep) = make_way(Vec2::ZERO, Vec2::Y, 0.0, &[Vec2::new(3.0, 2.0)]);
        assert_eq!((want, keep), (0.0, 1.0));
        let (want, keep) = make_way(Vec2::ZERO, Vec2::Y, 0.0, &[Vec2::new(0.0, -5.0)]);
        assert_eq!((want, keep), (0.0, 1.0), "behind it");
        assert_eq!(keep_clear(Vec2::ZERO, Vec2::Y, 0.3, &[]), 0.3);
    }

    #[test]
    fn a_walker_keeps_stepping_the_way_it_started() {
        // Dead ahead, it may pick either side, but once stepped left it
        // stays left as the person edges toward that side.
        let ahead = Vec2::new(0.0, 3.0);
        let (want, _) = make_way(Vec2::ZERO, Vec2::Y, -0.4, &[ahead]);
        assert!(want < 0.0);
        let edged = ahead + right_of(Vec2::Y) * -0.2;
        let (want, _) = make_way(Vec2::ZERO, Vec2::Y, -0.4, &[edged]);
        assert!(want < 0.0, "flipped to {want}");
    }

    #[test]
    fn about_four_in_ten_look_and_not_for_long() {
        let mut looked = 0;
        for w in 0..400u64 {
            for e in 0..5u32 {
                if let Some(s) = glance(w, e) {
                    looked += 1;
                    assert!((1.0..=2.5).contains(&s), "{s} s");
                }
            }
        }
        let share = looked as f32 / 2000.0;
        assert!((0.33..0.47).contains(&share), "{share}");
    }

    #[test]
    fn a_head_turns_toward_but_not_round_behind() {
        let f = Vec2::Y;
        // Straight ahead: no turn.
        assert!(head_toward(f, Vec2::new(0.0, 4.0)).unwrap().abs() < 1e-6);
        // The body's +x is world (f.y, −f.x) = +x here.
        let to_x = head_toward(f, Vec2::new(2.0, 2.0)).unwrap();
        assert!((to_x - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
        let to_minus_x = head_toward(f, Vec2::new(-2.0, 2.0)).unwrap();
        assert!(to_minus_x < 0.0);
        assert!(head_toward(f, Vec2::new(1.0, -1.0)).is_none(), "behind");
        assert!(
            head_toward(f, Vec2::new(1.0, 0.0)).is_none(),
            "90° is past the limit"
        );
        // And it turns at a bounded rate.
        let mut h = 0.0;
        for _ in 0..6 {
            h = turn_head(h, 1.0, 0.05);
        }
        assert!((h - 0.78).abs() < 1e-4, "{h}");
    }

    #[test]
    fn a_passer_by_looks_then_looks_ahead_again() {
        // Find a walker whose first encounter is a look.
        let walker = (0..100u64).find(|&w| glance(w, 1).is_some()).unwrap();
        let mut r = Reaction::default();
        let dt = 0.05;
        let person = [Vec2::new(1.0, 0.0)];
        let mut turned = 0.0_f32;
        // Approach from 4 m: within range, ahead and to the side.
        for k in 0..80 {
            let p = Vec2::new(0.0, -4.0 + k as f32 * 0.05);
            r.look(walker, p, Vec2::Y, &person, dt);
            turned = turned.max(r.head.abs());
            assert!(r.head.abs() <= HEAD_LIMIT + 1e-4);
        }
        assert!(turned > 0.2, "it never looked: {turned}");
        // Gone: the head comes back to the front.
        for _ in 0..40 {
            r.look(walker, Vec2::new(0.0, 20.0), Vec2::Y, &person, dt);
        }
        assert!(r.head.abs() < 1e-4);
        assert!(!r.meeting);
    }

    #[test]
    fn the_keenest_one_or_two_wave_back_after_a_pause() {
        // In front at 3, 5 and 8 m; one behind at 2 m, one out of reach.
        let facing = Vec2::new(0.0, -1.0);
        let waver = Vec2::ZERO;
        let walkers = [
            Vec2::new(0.0, 3.0),
            Vec2::new(0.5, 5.0),
            Vec2::new(-1.0, 8.0),
            Vec2::new(0.0, -2.0),
            Vec2::new(0.0, 14.0),
        ];
        // The one at -2 faces away from the waver: it cannot see it.
        let facings = [facing, facing, facing, Vec2::new(0.0, -1.0), facing];
        let scores: Vec<Option<f32>> = walkers
            .iter()
            .zip(facings)
            .map(|(&p, f)| wave_candidate(p, f, waver))
            .collect();
        assert!(scores[3].is_none(), "behind its back");
        assert!(scores[4].is_none(), "out of reach");
        let (mut ones, mut twos) = (0, 0);
        for wave in 0..300 {
            let picked = pick_wave_backs(
                scores
                    .iter()
                    .enumerate()
                    .filter_map(|(i, s)| s.map(|s| (i, s))),
                wave,
            );
            let (first, delay) = picked[0].expect("somebody answers");
            assert_eq!(first, 0, "the nearest in front answers first");
            assert!((0.45..=1.15).contains(&delay), "{delay} s");
            match picked[1] {
                Some((second, d2)) => {
                    twos += 1;
                    assert_eq!(second, 1);
                    assert!(d2 >= 0.75, "the second is a little later: {d2}");
                }
                None => ones += 1,
            }
        }
        assert!(twos > 60 && ones > 150, "{ones} once, {twos} twice");
        // Nobody about, nobody answers.
        assert_eq!(pick_wave_backs(std::iter::empty(), 1), [None, None]);
    }

    #[test]
    fn a_wave_back_notices_stops_waves_and_walks_on() {
        let secs = 1.6;
        let mut w = WaveBack {
            at: Vec2::ZERO,
            t: 0.2,
            delay: 0.6,
            waving: None,
        };
        assert_eq!(w.step(secs), WaveStep::Noticing);
        w.t = 1.0;
        assert_eq!(w.step(secs), WaveStep::Stopping);
        w.waving = Some(0.0);
        assert_eq!(
            w.step(secs),
            WaveStep::Waving {
                progress: 0.0,
                blend: 0.0
            }
        );
        w.waving = Some(1.0);
        let WaveStep::Waving { progress, blend } = w.step(secs) else {
            panic!("{:?}", w.step(secs))
        };
        assert!(blend == 1.0 && (0.4..0.6).contains(&progress), "{progress}");
        w.waving = Some(WAVE_BLEND_IN + secs + WAVE_BLEND_OUT * 0.5);
        let WaveStep::Waving { progress, blend } = w.step(secs) else {
            panic!()
        };
        assert!((progress - 1.0).abs() < 1e-6 && (blend - 0.5).abs() < 0.01);
        w.waving = Some(WAVE_BLEND_IN + secs + WAVE_BLEND_OUT + 0.01);
        assert_eq!(w.step(secs), WaveStep::Done);
        // One that never managed to stop gives up.
        let stuck = WaveBack {
            t: 0.6 + WAVE_GIVE_UP_S + 0.1,
            waving: None,
            ..w
        };
        assert_eq!(stuck.step(secs), WaveStep::Done);
    }
}
