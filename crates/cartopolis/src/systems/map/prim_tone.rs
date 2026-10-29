//! Tones prim scripts sound (`set_tone`, protocol 36): synthesised here, on
//! every client in range, never streamed as audio.
//!
//! The prim carries a [`ToneSpec`] (`components::SoundingTone`) and this keeps
//! one player per sounding prim within its range, attenuated by the camera's
//! distance exactly as `prim_sound` attenuates a loop.
//!
//! **Smoothing is the synthesiser's job, not the frame's.** A script following a
//! hand changes the tone up to twenty times a second. The main thread only
//! writes the target pitch and gain into atomics; the audio thread eases toward
//! them per sample — a glide in log-frequency at the script's `glide`, and a
//! 15 ms ramp on gain. Stepping a sink's volume per frame instead would put an
//! audible zipper on a pure tone, and every update would click.

// Without the `audio` feature nothing plays, but the synthesiser and its tests
// still compile — the same split `prim_sound` and `soundscape` make.
#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use bevy::prelude::*;
use cartopolis_core::tone::Voice;
use cartopolis_core::ToneSpec;

/// Output rate. Above twice the highest pitch a tone may take (4 kHz), with
/// room for the harmonics the voices carry.
pub const SAMPLE_RATE: u32 = 44_100;

/// Vibrato rate, Hz — a singer's, roughly.
const VIBRATO_HZ: f32 = 5.5;
/// Gain ramp time constant: fast enough to follow a hand, slow enough that a
/// step in level is not a click.
const GAIN_TAU_S: f32 = 0.015;
/// Shortest pitch ramp, even at `glide = 0`: a jump in pitch is a kink in the
/// waveform's slope, and that clicks.
const MIN_GLIDE_S: f32 = 0.004;
/// Output ceiling, leaving headroom so several tones and the soundscape
/// together do not clip.
const OUTPUT_PEAK: f32 = 0.35;

/// One voice, stepped a sample at a time. Pure, so it is tested here without an
/// audio device.
#[derive(Debug, Clone)]
pub struct Synth {
    phase: f32,
    lfo: f32,
    /// The sounding pitch's logarithm. `f64`, not `f32`: a 0.2 s glide moves
    /// it ~1e-4 of the remaining gap per sample, and in single precision that
    /// step falls under the value's own rounding once the gap is a few cents —
    /// the pitch stalled 2 Hz short of 880 and stayed there, audibly flat.
    log_hz: Option<f64>,
    gain: f32,
}

impl Default for Synth {
    fn default() -> Self {
        Self {
            phase: 0.0,
            lfo: 0.0,
            log_hz: None,
            gain: 0.0,
        }
    }
}

/// PolyBLEP: the correction that takes the alias out of a hard edge.
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

impl Synth {
    /// The next sample toward `hz` at `gain`.
    pub fn next(&mut self, hz: f32, gain: f32, voice: Voice, glide_s: f32, vibrato: f32) -> f32 {
        let sr = SAMPLE_RATE as f32;
        let target = f64::from(hz.clamp(1.0, sr * 0.45)).ln();
        // The first pitch is taken as it is; only changes glide.
        let log_hz = self.log_hz.get_or_insert(target);
        let tau = f64::from(glide_s.max(MIN_GLIDE_S));
        *log_hz += (target - *log_hz) * (1.0 - (-1.0 / (tau * f64::from(sr))).exp());
        let log_hz = *log_hz as f32;
        self.gain += (gain.clamp(0.0, 1.0) - self.gain) * (1.0 - (-1.0 / (GAIN_TAU_S * sr)).exp());

        self.lfo = (self.lfo + VIBRATO_HZ / sr).fract();
        let wobble = 2f32.powf(vibrato * (self.lfo * std::f32::consts::TAU).sin() / 12.0);
        let dt = (log_hz.exp() * wobble / sr).min(0.5);
        self.phase = (self.phase + dt).fract();

        let p = self.phase;
        let s = |k: f32| (p * k * std::f32::consts::TAU).sin();
        let wave = match voice {
            Voice::Sine => (s(1.0) + 0.15 * s(2.0) + 0.05 * s(3.0)) / 1.2,
            Voice::Organ => (s(1.0) + 0.5 * s(2.0) + 0.33 * s(3.0) + 0.25 * s(4.0)) / 2.08,
            Voice::Saw => 2.0 * p - 1.0 - poly_blep(p, dt),
            Voice::Square => {
                let raw = if p < 0.5 { 1.0 } else { -1.0 };
                raw + poly_blep(p, dt) - poly_blep((p + 0.5).fract(), dt)
            }
        };
        wave * self.gain * OUTPUT_PEAK
    }

    /// The pitch it is sounding now, without vibrato.
    #[cfg(test)]
    #[must_use]
    pub fn hz(&self) -> f32 {
        self.log_hz.map_or(0.0, |l| l.exp() as f32)
    }
}

/// Targets shared with the audio thread: written by a Bevy system, read by the
/// decoder. Atomics rather than a lock, because the audio thread must never
/// wait on the main thread.
#[derive(Debug, Default)]
pub struct ToneControl {
    pitch: AtomicU32,
    gain: AtomicU32,
    voice: AtomicU32,
    glide: AtomicU32,
    vibrato: AtomicU32,
}

impl ToneControl {
    /// Sound `tone` at `gain` (the tone's own gain, already attenuated).
    pub fn set(&self, tone: &ToneSpec, gain: f32) {
        self.pitch.store(tone.pitch_hz.to_bits(), Ordering::Relaxed);
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
        let voice = Voice::ALL
            .iter()
            .position(|v| *v == tone.voice)
            .unwrap_or(0) as u32;
        self.voice.store(voice, Ordering::Relaxed);
        self.glide.store(tone.glide_s.to_bits(), Ordering::Relaxed);
        self.vibrato
            .store(tone.vibrato.to_bits(), Ordering::Relaxed);
    }

    /// Fade to nothing at the pitch it has, over the synthesiser's gain ramp.
    pub fn silence(&self) {
        self.gain.store(0f32.to_bits(), Ordering::Relaxed);
    }

    fn read(&self) -> (f32, f32, Voice, f32, f32) {
        let f = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
        let voice = Voice::ALL
            .get(self.voice.load(Ordering::Relaxed) as usize)
            .copied()
            .unwrap_or(Voice::Sine);
        (
            f(&self.pitch),
            f(&self.gain),
            voice,
            f(&self.glide),
            f(&self.vibrato),
        )
    }
}

/// A playable stream of one prim's tone — an `Asset` because that is
/// bevy_audio's currency, as `voice::VoiceStream` is, which lets it share the
/// one output device everything else plays through.
#[derive(Asset, TypePath, Clone)]
pub struct ToneStream {
    control: Arc<ToneControl>,
}

/// The pull side of a [`ToneStream`]. Never ends: rodio drops a sink whose
/// source has, and silence is a gain of zero, not an end.
pub struct ToneDecoder {
    control: Arc<ToneControl>,
    synth: Synth,
    targets: (f32, f32, Voice, f32, f32),
    countdown: u32,
}

/// Samples between two reads of the targets: ~1.5 ms, far under what a hand
/// can do, and it keeps five atomic loads out of every sample.
const TARGET_READ_EVERY: u32 = 64;

impl Iterator for ToneDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.countdown == 0 {
            self.targets = self.control.read();
            self.countdown = TARGET_READ_EVERY;
        }
        self.countdown -= 1;
        let (hz, gain, voice, glide, vibrato) = self.targets;
        Some(self.synth.next(hz, gain, voice, glide, vibrato))
    }
}

#[cfg(feature = "audio")]
impl bevy::audio::Source for ToneDecoder {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> bevy::audio::ChannelCount {
        bevy::audio::ChannelCount::new(1).expect("1 is not zero")
    }

    fn sample_rate(&self) -> bevy::audio::SampleRate {
        bevy::audio::SampleRate::new(SAMPLE_RATE).expect("SAMPLE_RATE is not zero")
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        None
    }
}

#[cfg(feature = "audio")]
impl bevy::audio::Decodable for ToneStream {
    type Decoder = ToneDecoder;

    fn decoder(&self) -> Self::Decoder {
        ToneDecoder {
            control: Arc::clone(&self.control),
            synth: Synth::default(),
            targets: self.control.read(),
            countdown: 0,
        }
    }
}

/// How loud a tone is `distance_m` from its prim, 0..=1: the same squared
/// falloff a prim's sounds use, so a tone and a bell beside it fade together.
#[must_use]
pub fn listener_gain(range_m: f32, distance_m: f32) -> f32 {
    cartopolis_core::PrimSound {
        sound: cartopolis_core::SoundName::Chime,
        volume: 1.0,
        range_m,
    }
    .gain_at(distance_m)
}

/// A tone's player is started inside its range and stopped this far past it,
/// so standing on the edge does not start and stop it every frame.
const RANGE_HYSTERESIS: f32 = 1.1;

/// How long a stopped tone's player is kept while it fades: seven of the gain
/// ramp's time constants, by when it is under a thousandth of its level.
/// Despawning it at once cut the wave wherever it was, which clicks.
pub const FADE_OUT_S: f32 = 7.0 * GAIN_TAU_S;

/// The player for one prim's tone.
#[derive(Component)]
pub struct TonePlayer {
    prim: i64,
    control: Arc<ToneControl>,
    /// Seconds since its tone stopped, while it fades out.
    fading: Option<f32>,
}

/// Keep one player per sounding prim in range, and feed it this frame's tone.
#[cfg(feature = "audio")]
pub fn drive_prim_tones(
    mut commands: Commands,
    prefs: Res<crate::resources::AppPrefs>,
    camera: Query<&GlobalTransform, With<crate::components::FreeCamera>>,
    prims: Query<(
        &crate::components::Prim,
        &crate::components::SoundingTone,
        &GlobalTransform,
    )>,
    mut players: Query<(Entity, &mut TonePlayer)>,
    mut streams: ResMut<Assets<ToneStream>>,
    time: Res<Time>,
) {
    use bevy::audio::{AudioPlayer, PlaybackMode, PlaybackSettings};
    use std::collections::HashMap;

    let effects = prefs.effects_gain();
    let Ok(eye) = camera.single().map(GlobalTransform::translation) else {
        return;
    };
    // Each sounding prim: its tone, and the camera's distance to it.
    let mut wanted: HashMap<i64, (ToneSpec, f32)> = prims
        .iter()
        .filter_map(|(prim, tone, at)| Some((prim.id, (tone.0?, at.translation().distance(eye)))))
        .collect();

    for (entity, mut player) in &mut players {
        match wanted.remove(&player.prim) {
            Some((tone, d)) if d < tone.range_m * RANGE_HYSTERESIS => {
                player
                    .control
                    .set(&tone, tone.gain * listener_gain(tone.range_m, d) * effects);
                player.fading = None;
            }
            // Stopped, walked out of range, or the prim is gone: faded out,
            // then let go.
            _ => {
                if player.fading.is_none() {
                    player.control.silence();
                }
                let fading = player.fading.unwrap_or(0.0) + time.delta_secs();
                player.fading = Some(fading);
                if fading >= FADE_OUT_S {
                    commands.entity(entity).try_despawn();
                }
            }
        }
    }
    for (prim, (tone, d)) in wanted {
        if d >= tone.range_m {
            continue;
        }
        let control = Arc::new(ToneControl::default());
        control.set(&tone, tone.gain * listener_gain(tone.range_m, d) * effects);
        commands.spawn((
            AudioPlayer(streams.add(ToneStream {
                control: Arc::clone(&control),
            })),
            PlaybackSettings {
                mode: PlaybackMode::Remove,
                ..default()
            },
            TonePlayer {
                prim,
                control,
                fading: None,
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zero crossings per second of `samples`, which for these voices is twice
    /// the fundamental.
    fn crossings_hz(samples: &[f32]) -> f32 {
        let n = samples
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        n as f32 / 2.0 / (samples.len() as f32 / SAMPLE_RATE as f32)
    }

    fn run(synth: &mut Synth, hz: f32, gain: f32, voice: Voice, secs: f32) -> Vec<f32> {
        (0..(secs * SAMPLE_RATE as f32) as usize)
            .map(|_| synth.next(hz, gain, voice, 0.0, 0.0))
            .collect()
    }

    #[test]
    fn it_sounds_the_pitch_it_is_asked_for() {
        for voice in [Voice::Sine, Voice::Square, Voice::Saw] {
            let mut s = Synth::default();
            let _ = run(&mut s, 440.0, 1.0, voice, 0.1);
            let tone = run(&mut s, 440.0, 1.0, voice, 1.0);
            let hz = crossings_hz(&tone);
            assert!((hz - 440.0).abs() < 4.0, "{voice:?} sounded {hz} Hz");
        }
    }

    #[test]
    fn it_keeps_headroom_and_is_silent_at_zero_gain() {
        let mut s = Synth::default();
        for voice in Voice::ALL {
            let tone = run(&mut s, 880.0, 1.0, voice, 0.3);
            let peak = tone.iter().fold(0f32, |m, x| m.max(x.abs()));
            assert!(peak <= 0.5, "{voice:?} peaked at {peak}");
        }
        let mut s = Synth::default();
        let quiet = run(&mut s, 440.0, 0.0, Voice::Sine, 0.2);
        assert!(quiet.iter().all(|x| x.abs() < 1e-6));
    }

    #[test]
    fn a_glide_slides_rather_than_jumps() {
        let mut s = Synth::default();
        for _ in 0..SAMPLE_RATE {
            s.next(220.0, 1.0, Voice::Sine, 0.2, 0.0);
        }
        s.next(880.0, 1.0, Voice::Sine, 0.2, 0.0);
        // One sample after the pitch jumped two octaves, the tone has barely moved.
        assert!(s.hz() < 225.0, "jumped to {}", s.hz());
        // Ten time constants later it has arrived (one, 0.2 s, is ~63 % of the
        // way in log-frequency).
        for _ in 0..2 * SAMPLE_RATE {
            s.next(880.0, 1.0, Voice::Sine, 0.2, 0.0);
        }
        assert!((s.hz() - 880.0).abs() < 1.0, "{}", s.hz());
    }

    #[test]
    fn listeners_hear_it_fade_with_distance() {
        assert!((listener_gain(25.0, 0.0) - 1.0).abs() < 1e-6);
        assert!(listener_gain(25.0, 5.0) > listener_gain(25.0, 15.0));
        assert_eq!(listener_gain(25.0, 30.0), 0.0);
    }

    #[test]
    fn a_stopped_tone_fades_out_before_its_player_goes() {
        let control = Arc::new(ToneControl::default());
        control.set(&ToneSpec::default(), 1.0);
        let mut decoder = ToneDecoder {
            targets: control.read(),
            control: Arc::clone(&control),
            synth: Synth::default(),
            countdown: 0,
        };
        for _ in 0..SAMPLE_RATE / 5 {
            decoder.next();
        }
        control.silence();
        let fade: Vec<f32> = (0..(FADE_OUT_S * SAMPLE_RATE as f32) as usize)
            .map(|_| decoder.next().unwrap())
            .collect();
        // Still sounding just after the stop — a ramp, not a cut — and under a
        // thousandth of full level by the time the player is despawned.
        assert!(fade[..64].iter().any(|s| s.abs() > 0.1), "it cut off");
        let tail = fade[fade.len() - 64..]
            .iter()
            .fold(0f32, |m, s| m.max(s.abs()));
        assert!(tail < 1e-3 * OUTPUT_PEAK, "still at {tail} when it goes");
    }
}
