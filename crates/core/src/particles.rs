//! A prim's particle emitter: the parameters, and nothing that draws them.
//!
//! The shape of this struct is decided by where it has to survive: it is a field
//! of [`crate::PrimDto`], so it travels in every `WorldSnapshot`, it is persisted
//! as one row's column, a script writes it with `particles`, and the client
//! turns it into uniforms. That is four consumers, so it is a **flat set of plain
//! numbers with no options in it** — every field always has a value, every value
//! is finite and in range by the time it leaves [`ParticleEffect::sanitized`],
//! and the client can hand the whole thing to a shader without a decision.
//!
//! # Why the client draws no particle it is told about individually
//!
//! There is no per-particle data here and there deliberately never will be. The
//! client generates the particles in a vertex shader out of these numbers and the
//! clock, the same way `systems::grass` generates blades — a fixed shared mesh of
//! quad slots, one small uniform block, and nothing uploaded per frame. A
//! CPU-simulated system would rewrite a vertex buffer every frame, which is the
//! upload churn `docs/notes/upload-bursts.md` exists about, and it would have to
//! travel on the wire at the tick rate to look the same on two screens.
//!
//! The consequence to know: **the particles are not simulated, they are a
//! function of time**, so they cannot collide, cannot be individually addressed,
//! and two clients agree only because they compute the same closed form from the
//! same numbers. That is the trade this makes.
//!
//! # A burst is a level with a sequence, not an event
//!
//! An explosion is one-shot, but a one-shot *message* would be missed by anyone
//! whose snapshot lands a tick late, and there is no reliable channel here for a
//! thing that happens once. So a burst is the same struct with [`burst`] set,
//! carried for as long as the prim lives, and receivers play it once per
//! `(prim, sequence)` they have not seen — exactly the rule
//! [`crate::EmoteState`] follows, and for the same reason.
//!
//! [`burst`]: ParticleEffect::burst

use serde::{Deserialize, Serialize};

/// Most particles one emitter may ask for.
///
/// The cost is a mesh slot, not a simulation step: the client's shared quad pool
/// is allocated once at this size and every emitter draws a prefix of it, so the
/// ceiling bounds the *draw*, and raising it costs vertices in one buffer rather
/// than anything per emitter. 256 quads is a convincing fire and about 3 KB of
/// index buffer.
pub const MAX_PARTICLES: u16 = 256;

/// Longest a single particle may live. Longer than the visible span of any
/// plausible effect, and it is what bounds how far from its emitter a particle
/// can travel — which is what the client's culling bound is derived from.
pub const MAX_PARTICLE_LIFE_S: f32 = 30.0;

/// Fastest a particle may leave the emitter (m/s), and the matching ceiling on
/// the acceleration it may be given. Both exist so a script cannot ask for an
/// effect whose extent is a kilometre wide, which would defeat the culling bound
/// and put a full-screen sheet of blended quads on a phone.
pub const MAX_PARTICLE_SPEED: f32 = 200.0;
/// Ceiling on `|gravity|`, in m/s².
pub const MAX_PARTICLE_GRAVITY: f32 = 200.0;
/// Ceiling on a particle's drawn size and on the emitter's source radius, both in
/// metres.
pub const MAX_PARTICLE_SIZE: f32 = 64.0;

/// One prim's particle emitter.
///
/// Every field is a number a shader can consume directly. There is no "style" or
/// "preset" enum: a named style would be a second vocabulary to keep in step
/// across the script API, the wire and the shader, and the two presets that
/// matter are [`ParticleEffect::fire`] and [`ParticleEffect::blast`] — Rust
/// constructors of the same numbers, so a caller that wants a fire that is a bit
/// bluer edits a field rather than asking for a new style.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ParticleEffect {
    /// Particles alive at once. Clamped to [`MAX_PARTICLES`].
    pub count: u16,
    /// Seconds each particle lives. In a continuous emitter the particles are
    /// spread evenly over one lifetime, so `count / life` is the emission rate —
    /// there is no separate rate field, because two numbers that must agree are
    /// one number written twice.
    pub life: f32,
    /// Initial speed away from the emitter, m/s.
    pub speed: f32,
    /// Half-angle of the emission cone about the prim's up axis, in degrees.
    /// 0 is a beam, 90 is a hemisphere, 180 is a sphere (what a blast wants).
    pub spread_deg: f32,
    /// Constant acceleration on the up axis, m/s². **Positive is up**: fire rises,
    /// so the everyday value here is a buoyancy, not Earth's gravity. Sparks that
    /// fall want a negative one.
    pub gravity: f32,
    /// Radius of the sphere particles are born in, metres. A point source reads as
    /// a jet; a fire wants roughly the width of what is burning.
    pub source_radius: f32,
    /// Drawn size at birth and at death, metres. Smoke grows, sparks shrink.
    pub size_start: f32,
    pub size_end: f32,
    /// Linear-space colour at birth and at death. A fire is a hot yellow-white
    /// cooling to a dull red; the fade to nothing is [`Self::alpha`]'s job, not a
    /// fade to black, because a black particle in an additive blend is invisible
    /// and in an alpha blend is a smut.
    pub color_start: [f32; 3],
    pub color_end: [f32; 3],
    /// Overall opacity of the effect, 0–1. Each particle additionally fades in
    /// over the first tenth of its life and out over the last half — that curve
    /// is the shader's and is not configurable, because every value of it that is
    /// not "fade in fast, fade out slow" looks like a bug.
    pub alpha: f32,
    /// How much the particles wander off their straight line, in metres of
    /// displacement at one second. This is what separates a flame from a firework:
    /// a fire's particles curl.
    pub turbulence: f32,
    /// Play once instead of continuously — every particle is born at the same
    /// moment and the emitter is finished a lifetime later.
    ///
    /// See the module note: this is a level carried for as long as the prim
    /// lives, and [`Self::sequence`] is what tells a receiver that a *second*
    /// blast happened rather than the same one being re-described.
    pub burst: bool,
    /// Bumped by the author (or by `particle_burst`) every time the effect is
    /// (re)started. A receiver that diffs on the struct alone would never replay
    /// an identical blast; one that replays on every snapshot would restart the
    /// fire 20 times a second.
    pub sequence: u32,
}

impl Default for ParticleEffect {
    fn default() -> Self {
        Self::fire()
    }
}

impl ParticleEffect {
    /// A flame: hot core, rising, curling, cooling to red as it goes.
    #[must_use]
    pub fn fire() -> Self {
        Self {
            count: 96,
            life: 1.4,
            speed: 1.6,
            spread_deg: 22.0,
            gravity: 2.2,
            source_radius: 0.35,
            size_start: 0.75,
            size_end: 0.12,
            color_start: [4.0, 1.9, 0.45],
            color_end: [1.1, 0.12, 0.02],
            alpha: 0.85,
            turbulence: 0.9,
            burst: false,
            sequence: 0,
        }
    }

    /// An explosion: everything at once, outward in every direction, falling.
    #[must_use]
    pub fn blast() -> Self {
        Self {
            count: 200,
            life: 1.1,
            speed: 14.0,
            spread_deg: 180.0,
            gravity: -9.0,
            source_radius: 0.5,
            size_start: 1.5,
            size_end: 0.25,
            color_start: [7.0, 3.2, 0.9],
            color_end: [0.9, 0.14, 0.03],
            alpha: 1.0,
            turbulence: 0.5,
            burst: true,
            sequence: 1,
        }
    }

    /// The same effect with every field finite and inside its limit.
    ///
    /// Called at **every** boundary — the script API, the admin API, and the
    /// database read — rather than once where the value is thought to enter,
    /// because the failure this prevents is silent and permanent: a NaN reaching
    /// the shader is a particle at an undefined position, i.e. a triangle
    /// stretched across the whole screen with no error anywhere, and it would be
    /// persisted and replicated to everyone. Clamping rather than refusing keeps
    /// a script running (`set_color` follows the same rule), and the check has
    /// to come *before* the clamp because `f32::clamp` propagates NaN.
    #[must_use]
    pub fn sanitized(&self) -> Self {
        fn n(v: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                fallback
            }
        }
        fn rgb(c: [f32; 3]) -> [f32; 3] {
            // The ceiling is well above 1: these are linear-space *emissive*
            // colours and the hot core of a flame is meant to clip the tonemapper.
            [
                n(c[0], 0.0, 32.0, 1.0),
                n(c[1], 0.0, 32.0, 1.0),
                n(c[2], 0.0, 32.0, 1.0),
            ]
        }
        Self {
            count: self.count.clamp(1, MAX_PARTICLES),
            life: n(self.life, 0.05, MAX_PARTICLE_LIFE_S, 1.0),
            speed: n(self.speed, 0.0, MAX_PARTICLE_SPEED, 1.0),
            spread_deg: n(self.spread_deg, 0.0, 180.0, 20.0),
            gravity: n(
                self.gravity,
                -MAX_PARTICLE_GRAVITY,
                MAX_PARTICLE_GRAVITY,
                0.0,
            ),
            source_radius: n(self.source_radius, 0.0, MAX_PARTICLE_SIZE, 0.1),
            size_start: n(self.size_start, 0.01, MAX_PARTICLE_SIZE, 0.5),
            size_end: n(self.size_end, 0.01, MAX_PARTICLE_SIZE, 0.1),
            color_start: rgb(self.color_start),
            color_end: rgb(self.color_end),
            alpha: n(self.alpha, 0.0, 1.0, 1.0),
            turbulence: n(self.turbulence, 0.0, 16.0, 0.0),
            burst: self.burst,
            sequence: self.sequence,
        }
    }

    /// How far from the emitter a particle can get, in metres — the radius the
    /// client sizes its culling bound to.
    ///
    /// Ballistic reach plus the source sphere, the turbulence budget and half a
    /// particle, so the bound covers the drawn quad and not merely its centre. An
    /// over-estimate is a prim that is drawn when it need not be; an
    /// under-estimate is a fire that vanishes when its emitter leaves the frustum
    /// while its smoke is still on screen, which is the failure worth avoiding.
    #[must_use]
    pub fn reach_m(&self) -> f32 {
        let s = self.sanitized();
        let t = s.life;
        s.source_radius
            + s.speed * t
            + 0.5 * s.gravity.abs() * t * t
            + s.turbulence * t
            + 0.5 * s.size_start.max(s.size_end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_non_finite_without_propagating_it() {
        let poisoned = ParticleEffect {
            life: f32::NAN,
            speed: f32::INFINITY,
            color_start: [f32::NAN, 0.0, 0.0],
            ..ParticleEffect::fire()
        };
        let clean = poisoned.sanitized();
        assert!(clean.life.is_finite());
        assert!(clean.speed.is_finite());
        assert!(clean.color_start.iter().all(|c| c.is_finite()));
    }

    /// The clamp cannot stand in for the finite check — this is the exact trap
    /// `set_color` documents, in a second place.
    #[test]
    fn clamp_alone_would_have_kept_the_nan() {
        assert!(f32::NAN.clamp(0.0, 1.0).is_nan());
    }

    #[test]
    fn both_presets_survive_sanitising_unchanged() {
        for e in [ParticleEffect::fire(), ParticleEffect::blast()] {
            assert_eq!(e, e.sanitized(), "a preset must already be in range");
        }
    }

    #[test]
    fn reach_covers_a_ballistic_particle() {
        let blast = ParticleEffect::blast();
        // 14 m/s for 1.1 s is 15.4 m before gravity is counted at all.
        assert!(blast.reach_m() > 15.4, "reach was {}", blast.reach_m());
    }

    #[test]
    fn count_is_capped() {
        let e = ParticleEffect {
            count: 60_000,
            ..ParticleEffect::fire()
        };
        assert_eq!(e.sanitized().count, MAX_PARTICLES);
    }
}
