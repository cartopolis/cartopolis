//! The avatar's soundscape: footsteps timed off the gait, a landing thud, and
//! wind that rises with speed.
//!
//! # Why the sounds are synthesised rather than shipped
//!
//! Every one of them is generated into WAV bytes at startup by [`synth`] and
//! handed to `Assets<AudioSource>`. No `.wav` files enter the tree, which matters
//! more here than it looks: `.gitattributes` tracks audio by extension, so a
//! shipped footstep would be a Git LFS object, and a clone without `git lfs
//! install` gets a 130-byte pointer file instead — the failure mode CLAUDE.md
//! documents for the brand font and the star catalogue. A footstep that is four
//! lines of arithmetic cannot fail that way.
//!
//! They are placeholders in the sense that a recorded footstep would sound
//! better. They are *not* placeholders in the sense of being silent: a filtered
//! noise burst with a fast envelope is recognisably a foot on stone, and having
//! the timing right matters far more than having the timbre right — a correctly
//! timed synthetic step reads as walking, and a beautiful sample fired at the
//! wrong moment reads as broken.
//!
//! # Why footsteps are counted, not triggered by the animation
//!
//! Bevy's animation player exposes a clip's seek time, so the honest way to place
//! a footfall would be an event on the clip. The motion-capture clips carry no such
//! events, and adding them means marking every take by hand. Instead the step *rate* is
//! derived from the same ground speed that drives the clip's playback rate
//! ([`crate::systems::locomotion::clip_speed_for`]), so the two stay in phase by
//! construction: when the clip plays 1.4× faster, the steps come 1.4× faster.

// Without the `audio` feature there is no playback, so nothing in the crate calls
// the synthesis or the rate maths — but they still compile and are still covered
// by the suite, which is the whole reason they were split out of the systems. A
// host that cannot build the ALSA backend at all (the agent container) therefore
// still checks everything here except the ECS wiring.
#![cfg_attr(not(feature = "audio"), allow(dead_code))]

#[cfg(feature = "audio")]
use bevy::audio::{AudioPlayer, AudioSink, AudioSource, PlaybackMode, PlaybackSettings, Volume};
#[cfg(feature = "audio")]
use bevy::prelude::*;

#[cfg(feature = "audio")]
use crate::components::Avatar;
#[cfg(feature = "audio")]
use crate::resources::{AppPrefs, AvatarState, CameraMode, CameraState};
#[cfg(feature = "audio")]
use crate::systems::avatar::{AvatarAnimations, ClipGait};
use crate::systems::locomotion;
#[cfg(feature = "audio")]
use cartopolis_core::motion;

/// Sample rate for everything synthesised here. 22.05 kHz is plenty for noise
/// bursts and wind, and halves the bytes.
pub(crate) const SAMPLE_RATE: u32 = 22_050;

/// Volume of a footstep at a stroll and at a run. A run is not just faster, it
/// is heavier, and a constant-volume step under an accelerating gait is one of
/// those things that sounds wrong without being identifiable.
///
/// Lowered 2026-08-16 along with the timbre below. These are the *foreground* of
/// a soundscape that has no other content, and at the old levels a step was as
/// loud as a chat notification — for something that happens 2–3 times a second
/// for as long as anyone is moving.
const STEP_VOLUME_WALK: f32 = 0.10;
const STEP_VOLUME_RUN: f32 = 0.24;

/// How far either side of nominal a step's playback rate is nudged.
///
/// **This is the difference between footsteps and a metronome.** Every other
/// property of a real footfall varies a little — where the foot lands, how hard,
/// how the sole meets the stone — and the ear is far better at spotting an exact
/// repeat than at identifying what a footstep should sound like. With two fixed
/// samples strictly alternating at an exactly periodic rate, the pattern locks in
/// after about four steps and stops reading as a person walking; that is most of
/// what "cheap" means here. Four variants, a ±6 % speed nudge and a ±12 % level
/// nudge give a cycle long enough not to be heard as one.
const STEP_SPEED_JITTER: f32 = 0.06;
const STEP_LEVEL_JITTER: f32 = 0.12;

/// Loudest a landing can be, at [`motion::TERMINAL_FALL_SPEED`].
const LAND_VOLUME_MAX: f32 = 0.4;
/// Below this impact speed a landing makes no separate sound — stepping off a
/// kerb is a footstep, not an event.
const LAND_MIN_IMPACT: f32 = 3.0;

/// How loud an arrival is. Above a footstep — it is an event, and a rare one —
/// and below the landing thud, which is the loudest thing the avatar does.
const TELEPORT_VOLUME: f32 = 0.3;

/// Wind is silent at rest and reaches this at full flight speed.
const WIND_VOLUME_MAX: f32 = 0.18;

/// How fast the wind volume follows the speed. Slow: wind has mass, and a gust
/// that tracks the accelerometer exactly reads as a volume slider.
const WIND_RATE: f32 = 2.0;

#[cfg(feature = "audio")]
/// The synthesised clips, built once at startup.
#[derive(Resource)]
pub struct Soundscape {
    /// Four footsteps, cycled, so a walk does not sound like a machine. It was
    /// two, on the theory that the ear stops noticing at two; it does not — two
    /// alternating samples at an exact rate is a *rhythm*, and a rhythm is the
    /// one thing walking is not. See [`STEP_SPEED_JITTER`] for the other half.
    steps: [Handle<AudioSource>; 4],
    land: Handle<AudioSource>,
    wind: Handle<AudioSource>,
    /// The arrival whoosh, played by `announce_teleports` on every jump that did
    /// not ask to be silent (`geo_nav::Teleport::silent`).
    teleport: Handle<AudioSource>,
    /// The Settings ▸ Audio sound test. Public because it is played from the
    /// panel rather than from this module's own systems.
    pub test_tone: Handle<AudioSource>,
}

#[cfg(feature = "audio")]
/// Marks the looping wind emitter so its volume can be tracked to the speed.
#[derive(Component)]
pub struct WindSound;

#[cfg(feature = "audio")]
/// Build the clips and start the (silent) wind loop.
pub fn setup_soundscape(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
    let mut source = |bytes: Vec<u8>| {
        sources.add(AudioSource {
            bytes: bytes.into(),
        })
    };
    let soundscape = Soundscape {
        steps: [
            source(synth::footstep(0.88)),
            source(synth::footstep(1.06)),
            source(synth::footstep(0.97)),
            source(synth::footstep(1.18)),
        ],
        land: source(synth::landing()),
        wind: source(synth::wind()),
        teleport: source(synth::teleport()),
        test_tone: source(synth::test_tone()),
    };
    // The wind runs from startup at zero volume rather than being spawned when
    // motion begins: a looping source started mid-motion has an audible attack,
    // and the whole point is that it should arrive without ever being noticed.
    commands.spawn((
        AudioPlayer(soundscape.wind.clone()),
        PlaybackSettings {
            mode: PlaybackMode::Loop,
            volume: Volume::Linear(0.0),
            ..default()
        },
        WindSound,
    ));
    commands.insert_resource(soundscape);
}

#[cfg(feature = "audio")]
/// Play a footstep each time the avatar's stride advances, and a thud when it
/// lands.
pub fn play_avatar_sounds(
    mut commands: Commands,
    time: Res<Time>,
    camera_state: Res<CameraState>,
    avatar_state: Res<AvatarState>,
    soundscape: Res<Soundscape>,
    prefs: Res<AppPrefs>,
    gaits: Query<&AvatarAnimations, With<Avatar>>,
    mut stride: Local<f32>,
    mut foot: Local<usize>,
) {
    // Read once: a clip's volume is fixed when it is spawned, so a slider moved
    // mid-stride affects the next step rather than the one already sounding.
    let gain = prefs.effects_gain();
    // Only in Follow: the other modes decouple the camera from the avatar, and
    // footsteps from an avatar a kilometre away are a poltergeist.
    if camera_state.mode != CameraMode::Avatar {
        return;
    }

    // A landing is an edge published for one frame by whichever path owns
    // movement (`motion::step_fall`), so both run modes get it from one place.
    if avatar_state.landing_impact > LAND_MIN_IMPACT {
        let t = ((avatar_state.landing_impact - LAND_MIN_IMPACT)
            / (motion::TERMINAL_FALL_SPEED - LAND_MIN_IMPACT))
            .clamp(0.0, 1.0);
        commands.spawn((
            AudioPlayer(soundscape.land.clone()),
            PlaybackSettings {
                mode: PlaybackMode::Despawn,
                volume: Volume::Linear(LAND_VOLUME_MAX * t.max(0.25) * gain),
                ..default()
            },
        ));
        // Land at the start of a stride, so the first step after touching down is
        // a full one rather than whatever fraction was left over.
        *stride = 0.0;
    }

    // Timed by the legs, which a GPS walk moves too (`AvatarState::stride_speed`).
    let speed = avatar_state.stride_speed();
    if avatar_state.is_flying || !avatar_state.grounded || speed < locomotion::MOVING_EPS {
        // Keep the phase where it is: stopping mid-stride and starting again from
        // there is exactly what a person does.
        return;
    }

    // Steps per second, from the clip actually playing and the same ratio that
    // sets its playback rate — which is what keeps the sound on the foot.
    let (walk, run) = gaits
        .single()
        .map_or((ClipGait::CHARACTER_WALK, ClipGait::CHARACTER_RUN), |a| {
            a.gaits()
        });
    let gait = if locomotion::wants_run_clip(speed, true) {
        run
    } else {
        walk
    };
    let rate = gait.steps_per_sec(speed);
    *stride += rate * time.delta_secs();
    if *stride < 1.0 {
        return;
    }
    // Carry the remainder rather than zeroing: a step is due partway through the
    // frame that crosses the threshold, and discarding the overshoot quantises
    // every footfall to a frame boundary and drops up to a whole frame of phase
    // each time. At a run that is several percent of the step interval, jittered
    // by whatever the frame time happened to be — which is heard as sloppy
    // timing rather than as variation. A long frame is still not paid back as a
    // burst: past two whole strides the phase is reset instead of queued.
    if *stride >= 2.0 {
        *stride = 0.0;
    } else {
        *stride -= 1.0;
    }
    *foot = (*foot + 1) % soundscape.steps.len();

    let loudness = STEP_VOLUME_WALK
        + (STEP_VOLUME_RUN - STEP_VOLUME_WALK) * (speed / locomotion::RUN_SPEED).clamp(0.0, 1.0);
    // Nudge each step off nominal so the sequence never settles into a pattern.
    // Deterministic (a hash of the step counter, not a PRNG) for the same reason
    // the noise is: a sound that differs between runs cannot be tested, and the
    // ear cannot tell one source of variation from another anyway.
    let (speed_wobble, level_wobble) = step_wobble(*foot, *stride);
    commands.spawn((
        AudioPlayer(soundscape.steps[*foot].clone()),
        PlaybackSettings {
            mode: PlaybackMode::Despawn,
            volume: Volume::Linear(loudness * level_wobble * gain),
            speed: speed_wobble,
            ..default()
        },
    ));
}

#[cfg(feature = "audio")]
/// Play the arrival whoosh each time the avatar is placed somewhere rather than
/// having travelled there.
///
/// Its own system rather than a branch in [`play_avatar_sounds`], for two
/// reasons. That one returns early outside the follow camera, and a teleport is
/// worth hearing in every mode — in Fly and Earth it is the only confirmation
/// that the jump landed at all, since there is no avatar on screen to watch
/// arrive. And it reads the sequence counter rather than a one-frame flag
/// (`AvatarState::teleport_sequence`), so it cannot race the pose system for the
/// same edge.
pub fn announce_teleports(
    mut commands: Commands,
    avatar_state: Res<AvatarState>,
    soundscape: Res<Soundscape>,
    prefs: Res<AppPrefs>,
    mut last: Local<u32>,
) {
    if *last == avatar_state.teleport_sequence {
        return;
    }
    *last = avatar_state.teleport_sequence;
    commands.spawn((
        AudioPlayer(soundscape.teleport.clone()),
        PlaybackSettings {
            mode: PlaybackMode::Despawn,
            volume: Volume::Linear(TELEPORT_VOLUME * prefs.effects_gain()),
            ..default()
        },
    ));
}

/// Per-step playback-speed and level multipliers, both centred on 1.0 and
/// bounded by [`STEP_SPEED_JITTER`] / [`STEP_LEVEL_JITTER`].
///
/// `phase` is the stride remainder carried into this step, which is continuous
/// and unrepeating in a way the step counter is not — using it as the entropy
/// keeps the four-sample cycle from lining up with the wobble cycle and
/// producing an eight-step pattern instead of a four-step one.
#[must_use]
fn step_wobble(foot: usize, phase: f32) -> (f32, f32) {
    // Two decorrelated saw-shaped signals from the same input. The golden-ratio
    // multipliers are the standard trick for spreading a fractional sequence out
    // evenly instead of clustering it.
    let a = (phase * 0.618_034 + foot as f32 * 0.381_966).fract();
    let b = (phase * 0.381_966 + foot as f32 * 0.618_034 + 0.5).fract();
    (
        1.0 + (a * 2.0 - 1.0) * STEP_SPEED_JITTER,
        1.0 + (b * 2.0 - 1.0) * STEP_LEVEL_JITTER,
    )
}

#[cfg(feature = "audio")]
/// Track the wind's volume to the avatar's speed.
pub fn update_wind_volume(
    time: Res<Time>,
    camera_state: Res<CameraState>,
    avatar_state: Res<AvatarState>,
    prefs: Res<AppPrefs>,
    mut wind: Query<&mut AudioSink, With<WindSound>>,
    mut current: Local<f32>,
) {
    let target = if camera_state.mode == CameraMode::Avatar {
        wind_volume(avatar_state.ground_speed, avatar_state.is_flying)
    } else {
        0.0
    };
    *current += (target - *current) * (1.0 - (-WIND_RATE * time.delta_secs()).exp());
    if (target - *current).abs() < 1e-4 {
        *current = target;
    }
    // The preference scales the *sink*, not the ramp: the ramp is a physical
    // model of air and has to keep its own state, so a slider moved at speed
    // must not be smoothed in as though the wind had changed.
    let gain = prefs.effects_gain();
    for mut sink in wind.iter_mut() {
        sink.set_volume(Volume::Linear(*current * gain));
    }
}

/// Wind volume for a speed. Silent at a walk (nobody hears the wind strolling),
/// rising through a run, and loudest in flight.
#[must_use]
pub fn wind_volume(speed: f32, flying: bool) -> f32 {
    let full = if flying {
        locomotion::FLY_SPEED
    } else {
        locomotion::RUN_SPEED
    };
    // The everyday gait is the onset, so the wind arrives exactly when the
    // avatar starts doing something faster than walking.
    let onset = locomotion::WALK_SPEED;
    if speed <= onset {
        return 0.0;
    }
    let t = ((speed - onset) / (full - onset)).clamp(0.0, 1.0);
    // Squared, because loudness is perceived roughly logarithmically and a linear
    // ramp arrives far too early.
    WIND_VOLUME_MAX * t * t
}

/// Synthesis. Everything here returns a complete, decodable WAV file.
pub mod synth {
    use super::SAMPLE_RATE;

    /// Deterministic white noise. A real PRNG would be overkill and
    /// `Math::random`-style nondeterminism would make the sounds differ between
    /// runs for no benefit.
    fn noise(seed: &mut u32) -> f32 {
        // xorshift32
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        (*seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Wrap mono 16-bit samples in a WAV header. Hand-rolled because a WAV header
    /// is 44 bytes of known constants and pulling in an encoder to write them
    /// would be the larger dependency by far.
    pub(crate) fn wav(samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
        out.extend_from_slice(&2u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    fn to_i16(v: f32) -> i16 {
        (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
    }

    /// A footstep, in three parts: the **click** of the sole meeting the stone,
    /// the **body** the foot puts into the ground under it, and the **scuff** of
    /// the weight rolling forward off it. `pitch` shifts all three, which is what
    /// makes the variants sound like the same shoe landing slightly differently
    /// rather than like different objects.
    ///
    /// # Why two poles, and why the numbers are measured
    ///
    /// This was one pole at `a = 0.28` over 90 ms, and it did not sound like a
    /// foot. Measured, its zero-crossing rate was **~2.5 kHz** — a bright,
    /// broadband hiss with a long tail, i.e. a puff of air. Fired two to three
    /// times a second under a moving avatar it reads as wind rather than as
    /// walking, which is exactly how it was reported from a phone.
    ///
    /// A single pole rolls off at 6 dB/octave, so there is no cutoff low enough
    /// to darken it that does not also swallow it. The second pole doubles the
    /// slope; `a = 0.10` then puts the energy around 300–400 Hz — the band a heel
    /// on stone actually occupies — and `GAIN` puts the peak back where one pole
    /// had it. The faster decay (`exp(-26 t)` over 70 ms rather than `exp(-9 t)`
    /// over 90 ms) is the other half: what made it a *gust* was the tail.
    ///
    /// # Why it is more than that one burst now
    ///
    /// Filtered noise with one envelope is a *tap*. It is in the right frequency
    /// band and it lands at the right moment, and it still reads as synthetic,
    /// because a real footfall is not one event — the impact and the weight
    /// arriving behind it are separated by a few milliseconds, and the foot goes
    /// on making noise while the body passes over it. Two additions, both cheap:
    ///
    /// - a **body** partial, two decaying sines around 80–130 Hz. That band is
    ///   below anything the noise carries, so it costs nothing in the
    ///   zero-crossing budget that keeps a step from sounding like a hiss, and it
    ///   is what a step has that a tap has not.
    /// - a **scuff**, the same filtered noise at a quarter of the level, delayed
    ///   ~18 ms and decaying slowly. This is the part that stops a step ending
    ///   abruptly; an abrupt end is what the ear hears as a sample rather than as
    ///   a sound.
    pub fn footstep(pitch: f32) -> Vec<u8> {
        /// Two cascaded poles cost roughly `a` in amplitude each; this restores
        /// the peak rather than being a loudness choice (that is
        /// [`super::STEP_VOLUME_WALK`]).
        const GAIN: f32 = 3.0;
        /// Fundamental of the body thump (Hz) before `pitch`, with a second
        /// partial a little over an octave up. Not harmonic on purpose: a foot on
        /// stone excites a floor, and a floor is not a string.
        const BODY_HZ: f32 = 82.0;
        const BODY_HZ_2: f32 = 131.0;
        /// How far into the clip the scuff starts (seconds), and how loud it is
        /// against the impact.
        const SCUFF_DELAY: f32 = 0.018;
        const SCUFF_LEVEL: f32 = 0.28;
        /// Peak the finished clip is normalised to.
        ///
        /// Three summed parts have no analytic peak — it depends on where the
        /// noise happens to be when the body partial is at its loudest, which
        /// changes with `pitch`. Tuning constants until the headroom test passes
        /// would be fitting a number to a measurement that will move the next
        /// time any of the three is touched; scaling the finished clip is the
        /// same guarantee, stated once. Loudness stays entirely in
        /// [`super::STEP_VOLUME_WALK`], which is where a listener can reach it.
        const PEAK: f32 = 0.7;

        let secs = 0.14;
        let len = (SAMPLE_RATE as f32 * secs) as usize;
        let scuff_start = (SAMPLE_RATE as f32 * SCUFF_DELAY) as usize;
        let mut seed = 0x1234_5678;
        let mut lp = 0.0_f32;
        let mut lp2 = 0.0_f32;
        // The impact keeps the measured cutoff; the scuff is darker still, which
        // is what separates "the sole hits" from "the sole drags".
        let a = (0.10 * pitch).clamp(0.02, 0.9);
        let a_scuff = a * 0.55;
        let mut sp = 0.0_f32;
        let mut sp2 = 0.0_f32;
        let mut scuff_seed = 0x5EED_1234_u32.wrapping_add((pitch * 4096.0) as u32);

        let mixed: Vec<f32> = (0..len)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                // Impact: fast attack, exponential decay — a heel then nothing.
                // The decay is per *second* now rather than per clip fraction, so
                // lengthening the clip for the scuff does not stretch the click.
                let click_env = if t < 0.0015 {
                    t / 0.0015
                } else {
                    (-186.0 * (t - 0.0015)).exp()
                };
                lp += a * (noise(&mut seed) - lp);
                lp2 += a * (lp - lp2);
                let click = lp2 * GAIN * click_env;

                // Body: the weight arriving, an instant behind the click.
                let body_env = (-42.0 * t).exp() * (1.0 - (-t * 900.0).exp());
                let body = ((t * BODY_HZ * pitch * core::f32::consts::TAU).sin() * 0.62
                    + (t * BODY_HZ_2 * pitch * core::f32::consts::TAU).sin() * 0.30)
                    * body_env
                    * 0.55;

                // Scuff: the roll-off, quieter and darker, with a slow decay.
                let scuff = if i >= scuff_start {
                    let st = (i - scuff_start) as f32 / SAMPLE_RATE as f32;
                    sp += a_scuff * (noise(&mut scuff_seed) - sp);
                    sp2 += a_scuff * (sp - sp2);
                    sp2 * GAIN * SCUFF_LEVEL * (1.0 - (-st * 240.0).exp()) * (-34.0 * st).exp()
                } else {
                    0.0
                };

                click + body + scuff
            })
            .collect();

        let peak = mixed.iter().fold(0.0_f32, |a, b| a.max(b.abs()));
        let scale = if peak > 1e-6 { PEAK / peak } else { 0.0 };
        let samples: Vec<i16> = mixed.iter().map(|v| to_i16(v * scale)).collect();
        wav(&samples)
    }

    /// A landing: the footstep's shape, lower and longer, with a low sine thump
    /// under it for the weight.
    pub fn landing() -> Vec<u8> {
        let len = (SAMPLE_RATE as f32 * 0.25) as usize;
        let mut seed = 0x9E37_79B9;
        let mut lp = 0.0_f32;
        let samples: Vec<i16> = (0..len)
            .map(|i| {
                let t = i as f32 / len as f32;
                let secs = i as f32 / SAMPLE_RATE as f32;
                let env = if t < 0.01 { t / 0.01 } else { (-7.0 * t).exp() };
                lp += 0.12 * (noise(&mut seed) - lp);
                let thump = (secs * 55.0 * std::f32::consts::TAU).sin() * (-14.0 * t).exp();
                to_i16((lp * 0.8 + thump * 0.6) * env)
            })
            .collect();
        wav(&samples)
    }

    /// A teleport arrival, matched to the arrival pose
    /// (`avatar::TELEPORT_ARRIVE_SECS`): a rising whoosh that resolves onto a
    /// pitched note as the body settles.
    ///
    /// Two layers with opposite envelopes, which is what makes it read as an
    /// *arrival* rather than as a departure. The **noise** is band-passed by a
    /// resonant filter whose centre frequency sweeps upward and which fades out —
    /// air rushing in to fill the space. The **tone** is a pair of sines a fifth
    /// apart that fade *in* under it and decay away, so the ear hears the noise
    /// hand over to a pitch: something arrived and is now standing there.
    ///
    /// Peak is checked by the suite (`a_teleport_keeps_its_headroom`) for the
    /// reason every clip here is: clipping in the asset is distortion no volume
    /// slider can undo, and a resonant filter is the easiest way in this file to
    /// manufacture a peak far above the sum of its parts.
    pub fn teleport() -> Vec<u8> {
        const SECS: f32 = 0.45;
        let len = (SAMPLE_RATE as f32 * SECS) as usize;
        let mut seed = 0x2545_F491;
        // A two-pole state-variable band-pass, swept. One pole would not be
        // selective enough to be heard as a sweep at all — it would just be noise
        // getting quieter.
        let (mut low, mut band) = (0.0_f32, 0.0_f32);
        let samples: Vec<i16> = (0..len)
            .map(|i| {
                let t = i as f32 / len as f32;
                let secs = i as f32 / SAMPLE_RATE as f32;

                // 300 Hz → ~3.4 kHz over the clip, quadratically so most of the
                // travel is early, where the noise is loudest.
                let centre = 300.0 + 3_100.0 * t * t;
                let f = (2.0 * std::f32::consts::PI * centre / SAMPLE_RATE as f32).min(1.2);
                let q = 0.35; // damping; lower rings harder
                let input = noise(&mut seed);
                low += f * band;
                band += f * (input - low - q * band);
                // Fade in over the first 8 ms so the clip does not start on a
                // discontinuity, then away as the tone takes over.
                let rush_env = (t / 0.02).min(1.0) * (-4.5 * t).exp();
                let rush = band * 0.30 * rush_env;

                // …and the note that lands under it. A fifth, the same interval
                // the sound test uses, an octave down so it sits below the rush.
                let tone_env = (t * 3.0).min(1.0) * (-5.0 * (t - 0.25).max(0.0)).exp();
                let tone = ((secs * 440.0 * std::f32::consts::TAU).sin() * 0.16
                    + (secs * 659.25 * std::f32::consts::TAU).sin() * 0.10)
                    * tone_env;

                to_i16(rush + tone)
            })
            .collect();
        wav(&samples)
    }

    /// The Audio settings tab's sound test: a short two-tone chime.
    ///
    /// Deliberately **not** a sine sweep or a single sine. The question the test
    /// answers is "is sound reaching my ears from *this* application", and a pure
    /// tone is easy to mistake for tinnitus or a coincidental system sound, while
    /// a sweep tells you nothing extra unless you are checking frequency response.
    /// Two clearly-pitched notes (A5 then E6, a rising fifth) are unmistakably
    /// deliberate and unmistakably ours.
    ///
    /// Each note is enveloped: a 5 ms attack and a decay to silence. Without the
    /// attack the discontinuity at the start of a tone is an audible click, which
    /// on a *speaker test* is exactly the artefact that makes someone think their
    /// output is broken.
    pub fn test_tone() -> Vec<u8> {
        const NOTES: [f32; 2] = [880.0, 1318.5];
        const NOTE_SECS: f32 = 0.18;
        let note_len = (SAMPLE_RATE as f32 * NOTE_SECS) as usize;
        let attack = (SAMPLE_RATE as f32 * 0.005) as usize;

        let mut samples: Vec<i16> = Vec::with_capacity(note_len * NOTES.len());
        for freq in NOTES {
            for i in 0..note_len {
                let secs = i as f32 / SAMPLE_RATE as f32;
                let env = if i < attack {
                    i as f32 / attack as f32
                } else {
                    // Exponential decay reads as a struck note; linear reads as a
                    // fade-out, which is what a sound *card* dying sounds like.
                    (-6.0 * (i - attack) as f32 / note_len as f32).exp()
                };
                let v = (secs * freq * std::f32::consts::TAU).sin() * env * 0.35;
                samples.push((v.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16);
            }
        }
        wav(&samples)
    }

    /// Wind: two seconds of heavily low-passed noise, cross-faded into itself at
    /// the seam so the loop has no click. A click every two seconds is the single
    /// most noticeable artefact a looping ambience can have.
    ///
    /// # The gain is a measurement, and the old one was distortion
    ///
    /// This was `lp2 * 6.0`, which is **past full scale**: measured, the clip
    /// peaked at exactly 1.000 with 1.08 % of its samples hard-clamped by
    /// [`to_i16`] and an RMS of −8.2 dBFS. Clipping a low-passed rush is what
    /// turns it into a broadband roar — the harmonics a clamp generates are
    /// precisely the top end the two poles were there to remove — so the loop
    /// arrived as a wind *tunnel* however quietly it was then played. Volume
    /// cannot undo it: the distortion is baked into the asset, and
    /// [`super::WIND_VOLUME_MAX`] only scales it.
    ///
    /// `GAIN` now leaves ~3 dB of headroom (peak ≈ 0.46, RMS ≈ −17 dB) and the
    /// slower pole takes the rush down to ~120 Hz. The suite asserts the
    /// headroom, because nothing about the number is self-evident and the
    /// symptom of losing it is a sound that is wrong rather than absent.
    pub fn wind() -> Vec<u8> {
        const GAIN: f32 = 2.5;
        const POLE: f32 = 0.035;
        let len = (SAMPLE_RATE as f32 * 2.0) as usize;
        let fade = (SAMPLE_RATE as f32 * 0.25) as usize;
        let mut seed = 0x0BAD_F00D;
        let mut lp = 0.0_f32;
        let mut lp2 = 0.0_f32;
        let raw: Vec<f32> = (0..len + fade)
            .map(|_| {
                lp += POLE * (noise(&mut seed) - lp);
                lp2 += POLE * (lp - lp2); // two poles: a rush, not a hiss
                lp2 * GAIN
            })
            .collect();
        let samples: Vec<i16> = (0..len)
            .map(|i| {
                let v = if i < fade {
                    // Blend the tail back over the head so end meets start.
                    let t = i as f32 / fade as f32;
                    raw[i] * t + raw[len + i] * (1.0 - t)
                } else {
                    raw[i]
                };
                to_i16(v)
            })
            .collect();
        wav(&samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nobody hears the wind while strolling, and flight is louder than a sprint
    /// at the same nominal fraction — the two things that make it read as air
    /// rather than as a volume slider tied to the speedometer.
    #[test]
    fn wind_starts_at_a_run_and_peaks_in_flight() {
        assert_eq!(wind_volume(0.0, false), 0.0, "silent at rest");
        assert_eq!(
            wind_volume(locomotion::STROLL_SPEED, false),
            0.0,
            "silent at a stroll"
        );
        assert_eq!(
            wind_volume(locomotion::WALK_SPEED, false),
            0.0,
            "and still silent at the walk it starts from"
        );
        let run = wind_volume(locomotion::RUN_SPEED, false);
        assert!(run > 0.0, "audible at a run: {run}");
        assert!((run - WIND_VOLUME_MAX).abs() < 1e-5, "a run is full");

        let flight = wind_volume(locomotion::FLY_SPEED, true);
        assert!((flight - WIND_VOLUME_MAX).abs() < 1e-5);
        // The same speed reached in flight is a smaller fraction of flight's
        // range, so it must be quieter than it would be on foot.
        assert!(wind_volume(locomotion::RUN_SPEED, true) < run);
    }

    /// Volume never leaves 0..=max, whatever it is handed.
    #[test]
    fn wind_volume_is_bounded() {
        for speed in [-5.0, 0.0, 1.0, 100.0, 1e6] {
            for flying in [false, true] {
                let v = wind_volume(speed, flying);
                assert!((0.0..=WIND_VOLUME_MAX).contains(&v), "{speed} → {v}");
            }
        }
    }

    /// The synthesised bytes are a WAV file: right magic, right header fields,
    /// and a data chunk whose length matches what follows it. If this is wrong
    /// the failure is silence at runtime with nothing in the log, which is
    /// exactly the kind of thing worth a test.
    #[test]
    fn the_synthesised_clips_are_well_formed_wav() {
        for (name, bytes) in [
            ("footstep", synth::footstep(1.0)),
            ("landing", synth::landing()),
            ("wind", synth::wind()),
        ] {
            let b: &[u8] = &bytes;
            assert_eq!(&b[0..4], b"RIFF", "{name}: not a RIFF");
            assert_eq!(&b[8..12], b"WAVE", "{name}: not a WAVE");
            assert_eq!(
                u16::from_le_bytes([b[22], b[23]]),
                1,
                "{name}: must be mono"
            );
            assert_eq!(
                u32::from_le_bytes([b[24], b[25], b[26], b[27]]),
                SAMPLE_RATE,
                "{name}: sample rate"
            );
            let data_len = u32::from_le_bytes([b[40], b[41], b[42], b[43]]) as usize;
            assert_eq!(b.len(), 44 + data_len, "{name}: data length disagrees");
            assert!(data_len > 0, "{name}: no audio in it");
            assert_eq!(
                u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize,
                36 + data_len,
                "{name}: RIFF size disagrees"
            );
        }
    }

    /// The wind loop's start and end match, so the loop does not click. This is
    /// the one property of the wind a listener would notice immediately and the
    /// only reason the synthesis is more than three lines.
    #[test]
    fn the_wind_loop_does_not_click() {
        let bytes = synth::wind();
        let b: &[u8] = &bytes;
        let sample = |i: usize| i16::from_le_bytes([b[44 + i * 2], b[45 + i * 2]]) as i32;
        let n = (b.len() - 44) / 2;
        let seam = (sample(0) - sample(n - 1)).abs();
        // Compare against the signal's own scale rather than an absolute number:
        // a discontinuity only clicks if it is large relative to the material.
        let peak = (0..n).map(|i| sample(i).abs()).max().unwrap_or(1).max(1);
        assert!(
            seam * 4 < peak,
            "seam jump {seam} is large against a peak of {peak}"
        );
    }

    /// Read a synthesised clip's samples back as `-1..=1` floats.
    fn samples_of(bytes: &[u8]) -> Vec<f32> {
        let n = (bytes.len() - 44) / 2;
        (0..n)
            .map(|i| {
                f32::from(i16::from_le_bytes([bytes[44 + i * 2], bytes[45 + i * 2]])) / 32767.0
            })
            .collect()
    }

    /// **Nothing may reach full scale.** `to_i16` clamps, and a clamp on a
    /// low-passed noise bed is not a quiet distortion — it manufactures exactly
    /// the high harmonics the filter existed to remove, so an overdriven wind
    /// loop arrives as a roar at *any* playback volume, which is how this was
    /// reported from a phone. The wind was at peak 1.000 with 1.08 % of its
    /// samples clamped before 2026-08-16.
    ///
    /// Headroom rather than "no clipping": a clip that just touches 1.0 is one
    /// constant away from clamping again, and this is the test that would say so.
    #[test]
    fn no_synthesised_clip_runs_out_of_headroom() {
        for (name, bytes) in [
            ("footstep-low", synth::footstep(0.88)),
            ("footstep-mid", synth::footstep(0.97)),
            ("footstep-high", synth::footstep(1.18)),
            ("landing", synth::landing()),
            ("wind", synth::wind()),
            ("teleport", synth::teleport()),
            ("test tone", synth::test_tone()),
        ] {
            let s = samples_of(&bytes);
            let peak = s.iter().fold(0.0_f32, |a, b| a.max(b.abs()));
            assert!(
                peak <= 0.8,
                "{name} peaks at {peak:.3} — no headroom left before the clamp"
            );
            let clamped = s.iter().filter(|v| v.abs() > 0.999).count();
            assert_eq!(clamped, 0, "{name} has {clamped} clipped samples");
        }
    }

    /// The arrival is a *rise*: the noise layer's brightness climbs across the
    /// clip and the pitched layer arrives underneath it. Asserted on the halves
    /// rather than on the sound, because "whoosh, then landed" is a relation
    /// between two moments and either constant can be tuned alone.
    #[test]
    fn a_teleport_rises_and_then_lands_on_a_note() {
        let s = samples_of(&synth::teleport());
        assert!(!s.is_empty(), "a teleport that synthesised to nothing");
        let half = s.len() / 2;
        let brightness = |w: &[f32]| {
            let crossings = (1..w.len())
                .filter(|&i| (w[i] >= 0.0) != (w[i - 1] >= 0.0))
                .count();
            crossings as f32 / (w.len() as f32 / SAMPLE_RATE as f32) / 2.0
        };
        assert!(
            brightness(&s[..half]) < brightness(&s[half..]),
            "the sweep has to go up: {:.0} Hz then {:.0} Hz",
            brightness(&s[..half]),
            brightness(&s[half..])
        );
        // It must still be audible at the end — the tone is what says "arrived",
        // and an envelope that decays too early leaves only the rush.
        let tail = s[s.len() * 3 / 4..]
            .iter()
            .fold(0.0_f32, |a, b| a.max(b.abs()));
        assert!(tail > 0.02, "the arrival fades out too early: {tail:.3}");
        // …and it must not start on a discontinuity, which is an audible click.
        assert!(s[0].abs() < 0.02, "opens on a step of {:.3}", s[0]);
    }

    /// A footstep must be *dark*. The zero-crossing rate stands in for a
    /// spectrum: filtered noise crossing zero 2,500 times a second is a hiss, and
    /// a hiss repeated two to three times a second under a moving avatar is what
    /// got reported as wind. A foot on stone lives below ~600 Hz.
    #[test]
    fn a_footstep_is_a_thud_not_a_hiss() {
        for pitch in [0.88, 0.97, 1.06, 1.18] {
            let s = samples_of(&synth::footstep(pitch));
            let crossings = (1..s.len())
                .filter(|&i| (s[i] >= 0.0) != (s[i - 1] >= 0.0))
                .count();
            let secs = s.len() as f32 / SAMPLE_RATE as f32;
            let hz = crossings as f32 / secs / 2.0;
            assert!(hz < 600.0, "footstep({pitch}) crosses at {hz:.0} Hz");
            // And it has to still be *there*: an over-filtered step is silence
            // with extra steps.
            let peak = s.iter().fold(0.0_f32, |a, b| a.max(b.abs()));
            assert!(peak > 0.2, "footstep({pitch}) is inaudible at {peak:.3}");
        }
    }

    /// The footstep variants differ, or cycling them buys nothing.
    #[test]
    fn the_footstep_variants_are_not_the_same_sound() {
        let variants = [0.88, 1.06, 0.97, 1.18].map(synth::footstep);
        for (i, a) in variants.iter().enumerate() {
            for b in &variants[i + 1..] {
                assert_ne!(a, b, "two variants are the identical sound");
            }
        }
    }

    /// A footstep has a **body** — energy well below the impact's own band. This
    /// is the part that separates a step from a tap, and it is invisible to the
    /// zero-crossing test above (which only says the sound is not *bright*).
    /// Measured as the fraction of the clip's energy left after a low-pass that
    /// keeps roughly the bottom 200 Hz.
    #[test]
    fn a_footstep_has_weight_under_it() {
        for pitch in [0.88, 1.18] {
            let s = samples_of(&synth::footstep(pitch));
            // One pole at ~200 Hz for this sample rate.
            let a = 1.0 - (-core::f32::consts::TAU * 200.0 / SAMPLE_RATE as f32).exp();
            let mut lp = 0.0_f32;
            let (mut low, mut total) = (0.0_f32, 0.0_f32);
            for v in &s {
                lp += a * (v - lp);
                low += lp * lp;
                total += v * v;
            }
            let share = low / total.max(1e-9);
            assert!(
                share > 0.05,
                "footstep({pitch}) has {:.1}% of its energy below 200 Hz — no body",
                share * 100.0
            );
        }
    }

    /// A step does not end abruptly: there is still audible signal in its last
    /// third (the scuff). A clip that stops dead reads as a sample being cut off
    /// rather than as a foot leaving the ground.
    #[test]
    fn a_footstep_does_not_stop_dead() {
        let s = samples_of(&synth::footstep(1.0));
        let third = s.len() / 3;
        let rms = |w: &[f32]| (w.iter().map(|v| v * v).sum::<f32>() / w.len() as f32).sqrt();
        let head = rms(&s[..third]);
        let tail = rms(&s[third * 2..]);
        assert!(
            tail > head * 0.002,
            "the tail ({tail:.5}) is silence against the impact ({head:.5})"
        );
    }
}
