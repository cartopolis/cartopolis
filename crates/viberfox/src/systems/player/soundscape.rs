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
//! a footfall would be an event on the clip. The Quaternius clips carry no such
//! events, and adding them means editing 20 glTFs. Instead the step *rate* is
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
use crate::resources::{AvatarState, CameraMode, CameraState};
use crate::systems::locomotion;
#[cfg(feature = "audio")]
use viberfox_core::motion;

/// Sample rate for everything synthesised here. 22.05 kHz is plenty for noise
/// bursts and wind, and halves the bytes.
const SAMPLE_RATE: u32 = 22_050;

/// Strides per second at the walk and run clips' authored speeds. Two footfalls
/// per cycle, so these are twice the cycle rate: the `Walk` clip is 1.333 s and
/// the `Run` clip 0.8 s (`docs/notes/avatar-locomotion.md` lists the set).
const WALK_STEPS_PER_SEC: f32 = 1.5;
const RUN_STEPS_PER_SEC: f32 = 2.5;

/// Volume of a footstep at a walk and at a sprint. A sprint is not just faster,
/// it is heavier, and a constant-volume step under an accelerating gait is one of
/// those things that sounds wrong without being identifiable.
const STEP_VOLUME_WALK: f32 = 0.18;
const STEP_VOLUME_SPRINT: f32 = 0.42;

/// Loudest a landing can be, at [`motion::TERMINAL_FALL_SPEED`].
const LAND_VOLUME_MAX: f32 = 0.7;
/// Below this impact speed a landing makes no separate sound — stepping off a
/// kerb is a footstep, not an event.
const LAND_MIN_IMPACT: f32 = 3.0;

/// Wind is silent at rest and reaches this at full flight speed.
const WIND_VOLUME_MAX: f32 = 0.35;
/// How fast the wind volume follows the speed. Slow: wind has mass, and a gust
/// that tracks the accelerometer exactly reads as a volume slider.
const WIND_RATE: f32 = 2.0;

#[cfg(feature = "audio")]
/// The synthesised clips, built once at startup.
#[derive(Resource)]
pub struct Soundscape {
    /// Two footsteps, alternated, so a walk does not sound like a machine. Two is
    /// enough — the ear locks onto an exact repeat, and stops noticing at two.
    steps: [Handle<AudioSource>; 2],
    land: Handle<AudioSource>,
    wind: Handle<AudioSource>,
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
        steps: [source(synth::footstep(0.9)), source(synth::footstep(1.15))],
        land: source(synth::landing()),
        wind: source(synth::wind()),
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
    mut stride: Local<f32>,
    mut foot: Local<usize>,
) {
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
                volume: Volume::Linear(LAND_VOLUME_MAX * t.max(0.25)),
                ..default()
            },
        ));
        // Land at the start of a stride, so the first step after touching down is
        // a full one rather than whatever fraction was left over.
        *stride = 0.0;
    }

    let speed = avatar_state.ground_speed;
    if avatar_state.is_flying || !avatar_state.grounded || speed < locomotion::MOVING_EPS {
        // Keep the phase where it is: stopping mid-stride and starting again from
        // there is exactly what a person does.
        return;
    }

    // Steps per second, from the same ratio that sets the clip's playback rate —
    // which is what keeps the sound on the foot.
    let rate = if locomotion::wants_run_clip(speed, true) {
        RUN_STEPS_PER_SEC * (speed / locomotion::RUN_CLIP_SPEED)
    } else {
        WALK_STEPS_PER_SEC * (speed / locomotion::WALK_CLIP_SPEED)
    };
    *stride += rate * time.delta_secs();
    if *stride < 1.0 {
        return;
    }
    // Never fire a burst of steps after a long frame: the stride is a phase, not
    // a queue, and a hitch should cost a step rather than pay back four at once.
    *stride = 0.0;
    *foot = (*foot + 1) % soundscape.steps.len();

    let loudness = STEP_VOLUME_WALK
        + (STEP_VOLUME_SPRINT - STEP_VOLUME_WALK)
            * (speed / locomotion::SPRINT_SPEED).clamp(0.0, 1.0);
    commands.spawn((
        AudioPlayer(soundscape.steps[*foot].clone()),
        PlaybackSettings {
            mode: PlaybackMode::Despawn,
            volume: Volume::Linear(loudness),
            ..default()
        },
    ));
}

#[cfg(feature = "audio")]
/// Track the wind's volume to the avatar's speed.
pub fn update_wind_volume(
    time: Res<Time>,
    camera_state: Res<CameraState>,
    avatar_state: Res<AvatarState>,
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
    for mut sink in wind.iter_mut() {
        sink.set_volume(Volume::Linear(*current));
    }
}

/// Wind volume for a speed. Silent at a walk (nobody hears the wind strolling),
/// rising through a sprint, and loudest in flight.
#[must_use]
pub fn wind_volume(speed: f32, flying: bool) -> f32 {
    let full = if flying {
        locomotion::FLY_SPEED
    } else {
        locomotion::SPRINT_SPEED
    };
    let onset = locomotion::JOG_SPEED;
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
    fn wav(samples: &[i16]) -> Vec<u8> {
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

    /// A footstep: a short noise burst through a one-pole low-pass, with a very
    /// fast attack and a ~90 ms decay. `pitch` shifts the filter cutoff, which is
    /// what makes the two variants sound like the same shoe on the same stone
    /// rather than two different objects.
    pub fn footstep(pitch: f32) -> Vec<u8> {
        let len = (SAMPLE_RATE as f32 * 0.09) as usize;
        let mut seed = 0x1234_5678;
        let mut lp = 0.0_f32;
        let a = (0.28 * pitch).clamp(0.05, 0.9);
        let samples: Vec<i16> = (0..len)
            .map(|i| {
                let t = i as f32 / len as f32;
                // Fast attack, exponential decay — a heel then nothing.
                let env = if t < 0.02 { t / 0.02 } else { (-9.0 * t).exp() };
                lp += a * (noise(&mut seed) - lp);
                to_i16(lp * env)
            })
            .collect();
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

    /// Wind: two seconds of heavily low-passed noise, cross-faded into itself at
    /// the seam so the loop has no click. A click every two seconds is the single
    /// most noticeable artefact a looping ambience can have.
    pub fn wind() -> Vec<u8> {
        let len = (SAMPLE_RATE as f32 * 2.0) as usize;
        let fade = (SAMPLE_RATE as f32 * 0.25) as usize;
        let mut seed = 0x0BAD_F00D;
        let mut lp = 0.0_f32;
        let mut lp2 = 0.0_f32;
        let raw: Vec<f32> = (0..len + fade)
            .map(|_| {
                lp += 0.05 * (noise(&mut seed) - lp);
                lp2 += 0.05 * (lp - lp2); // two poles: a rush, not a hiss
                lp2 * 6.0
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
            wind_volume(locomotion::WALK_SPEED, false),
            0.0,
            "silent at a walk"
        );
        assert_eq!(
            wind_volume(locomotion::JOG_SPEED, false),
            0.0,
            "and still silent at the jog it starts from"
        );
        let sprint = wind_volume(locomotion::SPRINT_SPEED, false);
        assert!(sprint > 0.0, "audible at a sprint: {sprint}");
        assert!((sprint - WIND_VOLUME_MAX).abs() < 1e-5, "a sprint is full");

        let flight = wind_volume(locomotion::FLY_SPEED, true);
        assert!((flight - WIND_VOLUME_MAX).abs() < 1e-5);
        // The same speed reached in flight is a smaller fraction of flight's
        // range, so it must be quieter than it would be on foot.
        assert!(wind_volume(locomotion::SPRINT_SPEED, true) < sprint);
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

    /// The two footsteps differ, or alternating them buys nothing.
    #[test]
    fn the_two_footsteps_are_not_the_same_sound() {
        assert_ne!(
            synth::footstep(0.9),
            synth::footstep(1.15),
            "both feet make the identical sound"
        );
    }
}
