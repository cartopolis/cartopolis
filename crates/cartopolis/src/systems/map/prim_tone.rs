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

/// The theremin played end to end, with nobody holding a phone: a virtual
/// player — a connection of its own, standing at the cabinet — moves its hand
/// along the rod through a melody at [`cartopolis_core::tone::HAND_HZ`], the
/// rate `prim_hand` reports at; the real simulator runs the real
/// `theremin.lua`; a second connection, a listener two metres away, takes the
/// `PrimTone`s it hears and plays them through this module's own decoder into a
/// WAV file. Every seam between a hand and a speaker is in the path except the
/// two ends: the tracker that finds the hand (the offsets are computed here, as
/// `prim_hand` would send them) and the audio device (the samples go to a file).
///
/// Ignored, because it plays in real time — about ten seconds. Run it with
/// `cargo test -p cartopolis --lib virtual_player -- --ignored --nocapture`;
/// `CARTO_THEREMIN_WAV` names the file (default `theremin.wav` in the temp
/// directory), and the scene beside it — the prims and the hand — goes to the
/// same name with `.json`, for a capture to play back (`CARTO_SHOT_REPLAY`).
#[cfg(all(test, not(target_arch = "wasm32")))]
mod virtual_player {
    use super::*;
    use bytes::Bytes;
    use cartopolis_core::{
        decode_app_frame, encode_app_frame, AuthCredential, GeoAnchor, HandPose, NetMessage,
        PrimDto, PrimGeometry, PrimSurface, PROTOCOL_VERSION,
    };
    use futures_util::{SinkExt, StreamExt};
    use std::time::{Duration, Instant};
    use tokio::net::TcpStream;
    use tokio_util::codec::{Framed, LengthDelimitedCodec};

    type Client = Framed<TcpStream, LengthDelimitedCodec>;

    const SCRIPT: &str = include_str!("../../../../simulator/content/theremin.lua");
    /// `ROD`, `LOW`, `HIGH`, `PITCH_NEAR` and `PITCH_FAR` in `theremin.lua`,
    /// read back here to place the hand. Change them there and the notes
    /// asserted below move off their pitches.
    const ROD: [f32; 3] = [0.35, 0.45, 0.0];
    const LOW: f32 = 130.81;
    const HIGH: f32 = 1046.5;
    const PITCH_NEAR: f32 = 0.05;
    const PITCH_FAR: f32 = 0.6;
    /// Where the listener stands from the cabinet.
    const LISTENER_AT_M: f32 = 2.0;
    /// How long the hand takes to move from one note of the melody to the next.
    const SLIDE_S: f32 = 0.07;
    /// Seconds of silence before the hand arrives, and kept after it leaves —
    /// long enough for the script to notice it has gone (`GONE_AFTER`, 1 s).
    const LEAD_IN_S: f32 = 0.4;
    const TAIL_S: f32 = 2.0;

    /// Note, beats at 100 bpm, and seconds the hand takes to get there.
    ///
    /// A melody moves the hand little: three octaves span the rod's 55 cm, so
    /// Ode to Joy's fifth is 11 cm. So the hand first comes in far from the rod,
    /// sweeps up to it and back — the whole reach, and the whole idea of the
    /// instrument — and at the end sinks away from it again.
    const MELODY: &[(&str, f32, f32)] = &[
        ("A#3", 0.8, 0.0),
        ("C6", 2.0, 1.0),
        ("E5", 1.0, 0.4),
        ("E5", 1.0, SLIDE_S),
        ("F5", 1.0, SLIDE_S),
        ("G5", 1.0, SLIDE_S),
        ("G5", 1.0, SLIDE_S),
        ("F5", 1.0, SLIDE_S),
        ("E5", 1.0, SLIDE_S),
        ("D5", 1.0, SLIDE_S),
        ("C5", 1.0, SLIDE_S),
        ("C5", 1.0, SLIDE_S),
        ("D5", 1.0, SLIDE_S),
        ("E5", 1.0, SLIDE_S),
        ("E5", 1.5, SLIDE_S),
        ("D5", 0.5, SLIDE_S),
        ("D5", 2.0, SLIDE_S),
        ("A#3", 2.5, 1.2),
    ];
    const BEAT_S: f32 = 0.6;

    /// One note: its pitch, start and end in seconds from the start, and the
    /// slide into it.
    #[derive(Clone, Copy)]
    struct Note {
        hz: f32,
        start: f32,
        end: f32,
        slide: f32,
    }

    fn score() -> Vec<Note> {
        let mut t = LEAD_IN_S;
        MELODY
            .iter()
            .map(|&(note, beats, slide)| {
                let start = t;
                t += beats * BEAT_S;
                Note {
                    hz: cartopolis_core::tone::note_hz(note).unwrap(),
                    start,
                    end: t,
                    slide,
                }
            })
            .collect()
    }

    /// The pitch the hand is asking for at `t`, or `None` while it is away. A
    /// player slides between notes, and scoops up from a semitone below to
    /// sound a repeated one — a theremin cannot stop between them.
    fn hand_pitch(score: &[Note], t: f32) -> Option<f32> {
        let i = score.iter().position(|n| t >= n.start && t < n.end)?;
        let n = score[i];
        let from = match i.checked_sub(1).map(|p| score[p].hz) {
            Some(prev) if (prev - n.hz).abs() < 0.01 => n.hz / 2f32.powf(1.0 / 12.0),
            Some(prev) => prev,
            None => n.hz,
        };
        let k = ((t - n.start) / n.slide.max(1e-3)).clamp(0.0, 1.0);
        // Eased, as an arm moves: off slowly, and slowing into the note.
        let k = k * k * (3.0 - 2.0 * k);
        Some((from.ln() + (n.hz.ln() - from.ln()) * k).exp())
    }

    /// Where the hand must be for the script to sound `hz`: to the right of the
    /// rod, at the distance `theremin.lua` maps to that pitch, out of the
    /// loop's reach so it plays at full volume.
    fn hand_for(hz: f32) -> [f32; 3] {
        let t = (hz / LOW).ln() / (HIGH / LOW).ln();
        let d = PITCH_FAR - t * (PITCH_FAR - PITCH_NEAR);
        [ROD[0] + d, ROD[1], ROD[2]]
    }

    async fn connect(addr: &str) -> Client {
        let mut c = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            LengthDelimitedCodec::builder()
                .max_frame_length(32 << 20)
                .little_endian()
                .new_codec(),
        );
        send(
            &mut c,
            &NetMessage::ClientHello {
                protocol_version: PROTOCOL_VERSION,
                auth: AuthCredential::Password {
                    username: "dev".into(),
                    password: "dev".into(),
                },
            },
        )
        .await;
        let ack = decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap();
        assert!(matches!(ack, NetMessage::ServerHelloAck { .. }), "{ack:?}");
        c
    }

    async fn send<S>(c: &mut S, msg: &NetMessage)
    where
        S: futures_util::Sink<Bytes> + Unpin,
        S::Error: std::fmt::Debug,
    {
        c.send(Bytes::from(encode_app_frame(msg).unwrap()))
            .await
            .unwrap();
    }

    async fn recv_until<T>(
        c: &mut Client,
        what: &str,
        mut f: impl FnMut(NetMessage) -> Option<T>,
    ) -> T {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let frame = tokio::time::timeout_at(deadline, c.next())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .unwrap()
                .unwrap();
            if let Some(v) = f(decode_app_frame(&frame).unwrap()) {
                return v;
            }
        }
    }

    /// What the listener heard, and when.
    enum Heard {
        Tone(Option<ToneSpec>),
        Name(String),
    }

    /// 16-bit mono PCM at [`SAMPLE_RATE`].
    fn write_wav(path: &std::path::Path, samples: &[f32]) {
        let data = (samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + data as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data.to_le_bytes());
        for s in samples {
            out.extend_from_slice(
                &((s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16).to_le_bytes(),
            );
        }
        std::fs::write(path, out).unwrap();
    }

    #[tokio::test]
    #[ignore = "plays in real time; run by hand to hear the theremin"]
    async fn a_virtual_player_plays_the_theremin() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("theremin.db").to_str().unwrap().to_string();
        {
            let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
            cartopolis_simulator::ensure_account(&conn, "dev", "dev").unwrap();
        }
        let handle = cartopolis_simulator::serve(Arc::new(cartopolis_simulator::SimConfig {
            database_path: db,
            listen: "127.0.0.1:0".into(),
            ..cartopolis_simulator::SimConfig::default()
        }))
        .await
        .unwrap();
        let addr = handle.local_addr.to_string();

        // The `theremin` content pack — cabinet, stand, rod and loop — made as
        // a player would make it, and the cabinet's script stored. Every prim
        // as the server sent it goes into the replay.
        let mut player = connect(&addr).await;
        let mut upserts: Vec<(f32, PrimDto)> = Vec::new();
        let mut prim_id = None;
        for (n, spec) in cartopolis_simulator::content::pack("theremin")
            .unwrap()
            .prims
            .into_iter()
            .enumerate()
        {
            send(
                &mut player,
                &NetMessage::CreatePrim {
                    request_id: n as u32 + 1,
                    name: spec.name.clone().unwrap_or_default(),
                    position: Vec3::new(0.0, spec.altitude.unwrap_or(0.0), 0.0),
                    rotation: Vec3::from(spec.rotation.unwrap_or_default()),
                    scale: Vec3::from(spec.scale.unwrap_or([1.0; 3])),
                    color: spec.color.unwrap_or([0.5; 3]),
                    texture_id: None,
                    shape: spec.shape.clone().unwrap_or_else(|| "box".into()),
                    surface: PrimSurface::default(),
                    geometry: PrimGeometry::default(),
                    geo: GeoAnchor {
                        latitude: spec.latitude.unwrap(),
                        longitude: spec.longitude.unwrap(),
                    },
                    particles: None,
                    model: None,
                },
            )
            .await;
            let prim = recv_until(&mut player, "a theremin part", |m| match m {
                NetMessage::PrimUpsert { prim } => Some(prim),
                _ => None,
            })
            .await;
            if let Some(source) = spec.script {
                assert_eq!(source, SCRIPT, "the pack plays another script");
                prim_id = Some(prim.id);
                send(
                    &mut player,
                    &NetMessage::SetPrimScript {
                        request_id: 100,
                        prim_id: prim.id,
                        source,
                    },
                )
                .await;
            }
            upserts.push((0.0, prim));
        }
        let prim_id = prim_id.expect("the pack has no scripted cabinet");
        let cabinet = recv_until(&mut player, "the script's sense_hands", |m| match m {
            NetMessage::PrimUpsert { prim } if prim.id == prim_id && prim.hand_range > 0.0 => {
                Some(prim)
            }
            _ => None,
        })
        .await;
        upserts.push((0.0, cabinet));

        let mut listener = connect(&addr).await;
        let score = score();
        let end = score.last().unwrap().end;
        let start = Instant::now();

        // The player's hand, sent at the rate a client reports one. Its own
        // incoming traffic is drained so the server never waits on it.
        let (mut tx, mut rx) = player.split();
        tokio::spawn(async move { while rx.next().await.is_some() {} });
        let hand_score = score.clone();
        let hand = tokio::spawn(async move {
            let mut sent = Vec::new();
            let mut tick = tokio::time::interval(Duration::from_secs_f32(
                1.0 / cartopolis_core::tone::HAND_HZ,
            ));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let t = start.elapsed().as_secs_f32();
                if t > end {
                    return sent;
                }
                let Some(hz) = hand_pitch(&hand_score, t) else {
                    continue;
                };
                let pose = HandPose {
                    prim_id,
                    offset: hand_for(hz),
                    precise: true,
                };
                send(&mut tx, &NetMessage::HandPose { pose }).await;
                sent.push((t, hz));
            }
        });

        let mut heard: Vec<(f32, Heard)> = Vec::new();
        let stop = tokio::time::Instant::now() + Duration::from_secs_f32(end + TAIL_S);
        while let Ok(Some(Ok(frame))) = tokio::time::timeout_at(stop, listener.next()).await {
            let t = start.elapsed().as_secs_f32();
            match decode_app_frame(&frame).unwrap() {
                NetMessage::PrimTone { prim_id: id, tone } if id == prim_id => {
                    heard.push((t, Heard::Tone(tone.map(ToneSpec::sanitized))));
                }
                NetMessage::PrimUpsert { prim } if prim.id == prim_id => {
                    heard.push((t, Heard::Name(prim.name.clone())));
                    upserts.push((t, prim));
                }
                _ => {}
            }
        }
        let sent = hand.await.unwrap();
        let total = start.elapsed().as_secs_f32();

        // Played as `drive_prim_tones` plays it: a player spawned at the first
        // tone, fed each tone as it arrives, faded out and despawned when it
        // stops.
        let mut samples = Vec::with_capacity((total * SAMPLE_RATE as f32) as usize);
        let mut decoder: Option<ToneDecoder> = None;
        let mut stopped_at: Option<f32> = None;
        let mut next = 0;
        while samples.len() < samples.capacity() {
            let t = samples.len() as f32 / SAMPLE_RATE as f32;
            if stopped_at.is_some_and(|s| t - s >= FADE_OUT_S) {
                decoder = None;
                stopped_at = None;
            }
            while next < heard.len() && heard[next].0 <= t {
                if let Heard::Tone(tone) = &heard[next].1 {
                    match tone {
                        Some(tone) => {
                            let d = decoder.get_or_insert_with(|| {
                                let control = Arc::new(ToneControl::default());
                                control.set(tone, 0.0);
                                ToneDecoder {
                                    targets: control.read(),
                                    control,
                                    synth: Synth::default(),
                                    countdown: 0,
                                }
                            });
                            d.control
                                .set(tone, tone.gain * listener_gain(tone.range_m, LISTENER_AT_M));
                            stopped_at = None;
                        }
                        None => {
                            if let Some(d) = &decoder {
                                d.control.silence();
                                stopped_at.get_or_insert(t);
                            }
                        }
                    }
                }
                next += 1;
            }
            samples.push(decoder.as_mut().and_then(Iterator::next).unwrap_or(0.0));
        }
        let path = std::env::var_os("CARTO_THEREMIN_WAV")
            .map_or_else(|| std::env::temp_dir().join("theremin.wav"), Into::into);
        write_wav(&path, &samples);
        // The scene beside it, for a capture to play back (`systems::shot_replay`).
        let replay = serde_json::json!({
            "prims": upserts
                .iter()
                .map(|(t, prim)| serde_json::json!({ "t": t, "prim": prim }))
                .collect::<Vec<_>>(),
            "hand": sent
                .iter()
                .map(|&(t, hz)| serde_json::json!({ "t": t, "prim_id": prim_id, "offset": hand_for(hz) }))
                .collect::<Vec<_>>(),
        });
        std::fs::write(
            path.with_extension("json"),
            serde_json::to_vec(&replay).unwrap(),
        )
        .unwrap();

        // What happened, for the person running it.
        let tones: Vec<(f32, ToneSpec)> = heard
            .iter()
            .filter_map(|(t, h)| match h {
                Heard::Tone(Some(s)) => Some((*t, *s)),
                _ => None,
            })
            .collect();
        println!("wrote {} ({total:.1} s)", path.display());
        println!(
            "hand reports sent: {}, tones heard: {}",
            sent.len(),
            tones.len()
        );
        for (t, h) in &heard {
            if let Heard::Name(n) = h {
                println!("{t:6.2} s  name: {n}");
            }
        }
        // Per note: from the hand reaching it to the listener hearing it.
        let mut latencies = Vec::new();
        for &Note {
            hz,
            start: s,
            end: e,
            slide,
        } in &score
        {
            let reached = s + slide;
            let first_sent = sent
                .iter()
                .find(|&&(t, p)| t >= reached && (p - hz).abs() < 0.01);
            // After the hand got there: a repeated note scoops away from the
            // pitch and back, and the note before it is still at that pitch.
            let first_heard = first_sent.and_then(|&(ts, _)| {
                tones.iter().find(|(t, spec)| {
                    *t >= ts && *t < e + 0.3 && (spec.pitch_hz / hz - 1.0).abs() < 0.005
                })
            });
            match (first_sent, first_heard) {
                (Some(&(ts, _)), Some(&(th, spec))) => {
                    latencies.push(th - ts);
                    println!(
                        "{s:6.2} s  {hz:7.2} Hz  heard {:7.2} Hz gain {:.2}  {:4.0} ms after the hand",
                        spec.pitch_hz,
                        spec.gain,
                        (th - ts) * 1000.0
                    );
                }
                _ => println!("{s:6.2} s  {hz:7.2} Hz  NOT HEARD"),
            }
        }
        latencies.sort_by(f32::total_cmp);
        if let (Some(lo), Some(hi)) = (latencies.first(), latencies.last()) {
            println!(
                "hand → ear: median {:.0} ms, {:.0}–{:.0} ms",
                latencies[latencies.len() / 2] * 1000.0,
                lo * 1000.0,
                hi * 1000.0
            );
        }

        assert_eq!(latencies.len(), score.len(), "a note went unheard");
        assert!(
            heard
                .iter()
                .any(|(_, h)| matches!(h, Heard::Name(n) if n.contains("dev is playing"))),
            "the script never named its player"
        );
        let last_hand = sent.last().unwrap().0;
        assert!(
            heard
                .iter()
                .any(|(t, h)| *t > last_hand && matches!(h, Heard::Tone(None))),
            "the tone did not stop after the hand left"
        );
        let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.1, "the file is silent: peak {peak}");
    }
}
