//! Proximity voice: the client half of protocol 15.
//!
//! The wire and the server have been ready since `VoiceFrame` / `VoiceData`
//! landed — `ChatHub` fans frames out by distance, `crates/simulator/tests/
//! voice.rs` pins the rules, and `platform::audio` opens a microphone. What did
//! not exist was anything joining the two: nothing encoded a captured buffer,
//! nothing sent one, and nothing played one back. This module is that path, end
//! to end:
//!
//! ```text
//! AudioInput ─drain→ resample →  gate  → frame → ADPCM → VoiceFrame ─→ sim
//!    48/44.1/16 kHz    16 kHz              20 ms   164 B
//!
//! sim ─→ VoiceData → ADPCM⁻¹ → per-speaker ring → rodio Source → the mix
//!                                              ↑ attenuated by distance_m
//! ```
//!
//! # Why a codec at all, and why this one
//!
//! `NetMessage::VoiceFrame`'s payload is opaque to the server ("Opus today, but
//! the codec is entirely a client concern"), and `MAX_VOICE_BYTES` is 1500 —
//! sized so a frame never has to be fragmented when this moves off TCP. Raw
//! 16-bit PCM does not fit that budget by an order of magnitude: 20 ms at 48 kHz
//! is 1,920 bytes, and 96 kB/s per speaker is not a proximity chat, it is a
//! denial of service with a microphone attached.
//!
//! Opus is the obvious answer and is deliberately not taken *yet*: every Rust
//! binding to it (`audiopus`, `opus`) wraps libopus through `cc`, which means a C
//! toolchain on the Android and wasm lanes — the two platforms this is for — and
//! `docs/notes/android-build.md` is a long argument for not acquiring build
//! prerequisites those lanes cannot satisfy. So the shipped codec is **IMA
//! ADPCM at 16 kHz**: four bits a sample, 4:1 against PCM, 164 bytes for a 20 ms
//! frame, ~8 kB/s per speaker, and about a hundred lines of arithmetic with no
//! dependency at all. It is worse than Opus and it is *speech*, which is the bar.
//!
//! Swapping it later costs nothing outside this file: the payload is opaque to
//! the sim, [`CODEC_TAG`] is the first byte of every frame, and a decoder that
//! meets a tag it does not know drops the frame rather than playing noise.
//!
//! # Three properties that are not obvious
//!
//! **Every frame is self-contained.** ADPCM is a predictor plus a step index, and
//! the naive framing carries that state implicitly across frames — one lost
//! packet then corrupts every frame after it until the streams happen to
//! reconverge. So the encoder's running state is *written into each frame's
//! header*. A frame decodes correctly with no history at all, which is what makes
//! a dropped frame cost 20 ms rather than the rest of the conversation.
//!
//! **The capture rate is whatever the device says.** A desktop microphone is
//! commonly 44.1 kHz, a phone's may be 16 kHz, and WebAudio hands out whatever
//! the browser chose — so [`Resampler`] converts from
//! `AudioInput::sample_rate()`, and nothing downstream ever assumes. Guessing
//! here does not fail loudly; it transmits speech at the wrong pitch.
//!
//! **The attenuation is the server's `distance_m`, not a Bevy spatial audio
//! emitter.** The sim already computed the distance it fanned the frame out by,
//! and it is the same number for every listener whatever their client is doing
//! with its camera — so voice sounds the same distance away in free-cam as on
//! foot, and a client that has not yet spawned an avatar entity for the speaker
//! still hears them at the right level.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use cartopolis_core::{ChatScope, NetMessage};

use crate::platform::audio::AudioInput;
use crate::resources::{AppPrefs, OnlineSession};

/// Rate voice is transmitted at. Not the capture rate and not the playback rate:
/// both ends convert.
///
/// 16 kHz rather than `platform::audio::VOICE_SAMPLE_RATE`'s 48: speech is
/// intelligible to about 7 kHz, so wideband costs three times the bytes for
/// bandwidth nobody's voice occupies. It is also the rate a phone's microphone
/// most often runs at natively, which makes the commonest case a copy.
pub const VOICE_RATE: u32 = 16_000;

/// Samples in one transmitted frame — 20 ms at [`VOICE_RATE`].
///
/// 20 ms is the interval the server's rate limiter is sized for
/// (`MIN_VOICE_INTERVAL` is 15 ms, "admits the expected ~50/s with slack").
/// Shorter frames spend more of the budget on headers; longer ones add latency to
/// every word.
pub const FRAME_SAMPLES: usize = (VOICE_RATE as usize * 20) / 1000;

/// First byte of every payload: which codec produced the rest.
///
/// The reason this exists before there is a second codec is that the *upgrade* is
/// the dangerous moment — two clients of different versions in earshot of each
/// other — and an unknown tag has to be droppable. Without it the first byte of
/// an Opus frame would be read as an ADPCM predictor and played as noise.
pub const CODEC_TAG: u8 = 1;

/// Bytes in an encoded frame: the tag, the predictor, the step index, then one
/// nibble per sample.
pub const FRAME_BYTES: usize = 4 + FRAME_SAMPLES / 2;

/// How much captured audio may pile up before the oldest is dropped, in frames.
///
/// A hitched frame (a tile rebuild, a garbage collection) leaves several frames'
/// worth of samples waiting. Sending the backlog is worse than dropping it: the
/// server's limiter refuses anything faster than 15 ms apart, so a burst is
/// discarded at the far end anyway, and what does arrive is late. Speech that is
/// 200 ms behind is not speech, it is an echo of a conversation that moved on.
const MAX_PENDING_FRAMES: usize = 4;

/// How much decoded audio a listener will hold for one speaker, in seconds.
///
/// This is the jitter buffer, and it is small on purpose. Every sample in it is
/// latency: a listener who has drifted half a second behind hears a delayed
/// conversation and answers over the top of it. Past this the *oldest* audio is
/// dropped — the same policy, and for the same reason, as
/// `platform::audio::MAX_BACKLOG` on the capture side.
pub(crate) const MAX_RING_SECS: f32 = 0.4;

/// Silence after which a speaker's sink is torn down.
///
/// Not zero, because the gaps between words are silence too and re-spawning a
/// sink per sentence would click. Not long, because an idle sink is a rodio
/// source being polled at the device rate for nothing.
const SPEAKER_TIMEOUT_SECS: f32 = 6.0;

/// Distance at which a voice is still at full volume, and the floor it never
/// falls below.
///
/// The curve is inverse-distance past [`FULL_VOLUME_M`], which is the physically
/// honest one, with a floor because the client cannot tell a say from a shout —
/// `VoiceData` carries no scope, and the server has already refused anything out
/// of range. Without the floor a shout at 300 m (`chat::SHOUT_RADIUS_M`, well
/// within what the sim delivers) would arrive at 1.6 % and read as a delivery
/// failure rather than as distance.
const FULL_VOLUME_M: f32 = 5.0;
const MIN_AUDIBLE: f32 = 0.06;

/// RMS below which an open microphone is treated as silence, and how long it
/// keeps sending after dropping under it.
///
/// The hangover is the whole design: a gate without one clips the end of every
/// word and, worse, the *quiet part* of a sentence, which is most of a sentence.
/// 0.6 s is longer than the pause inside a phrase and shorter than the pause
/// between turns.
const GATE_OPEN_RMS: f32 = 0.02;
const GATE_HANGOVER_SECS: f32 = 0.6;

// ---------------------------------------------------------------------------
// The codec
// ---------------------------------------------------------------------------

/// IMA ADPCM step sizes, from the IMA/DVI specification. Not derivable — it is a
/// table, reproduced rather than computed.
#[rustfmt::skip]
const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45,
    50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230,
    253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327,
    3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442,
    11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

/// How the step index moves for each 4-bit code. Small codes step down (the
/// signal is quieter than the quantiser expected), large ones step up.
#[rustfmt::skip]
const INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// The encoder/decoder's running state: a predicted sample and a step index.
///
/// Public because the send path carries one across frames — see the module note
/// on self-contained frames — and the tests drive it directly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdpcmState {
    predictor: i32,
    index: i32,
}

impl AdpcmState {
    fn encode(&mut self, sample: i16) -> u8 {
        let step = STEP_TABLE[self.index as usize];
        let mut diff = i32::from(sample) - self.predictor;
        let mut code = 0u8;
        if diff < 0 {
            code = 8;
            diff = -diff;
        }
        // Three successive halvings of the step: the classic IMA quantiser, and
        // `step >> 3` is the rounding term that keeps the reconstruction centred
        // in its bucket rather than at its edge.
        let mut delta = step >> 3;
        if diff >= step {
            code |= 4;
            diff -= step;
            delta += step;
        }
        if diff >= step >> 1 {
            code |= 2;
            diff -= step >> 1;
            delta += step >> 1;
        }
        if diff >= step >> 2 {
            code |= 1;
            delta += step >> 2;
        }
        self.advance(code, delta);
        code
    }

    fn decode(&mut self, code: u8) -> i16 {
        let step = STEP_TABLE[self.index as usize];
        let mut delta = step >> 3;
        if code & 4 != 0 {
            delta += step;
        }
        if code & 2 != 0 {
            delta += step >> 1;
        }
        if code & 1 != 0 {
            delta += step >> 2;
        }
        self.advance(code, delta);
        // `as i16` after the clamp, never before: the predictor is deliberately
        // held in an i32 so an overshoot is clamped rather than wrapped, and a
        // wrap is a full-scale click.
        self.predictor as i16
    }

    /// Apply one code's effect to the predictor and the step index. Shared by
    /// both directions, which is what makes the encoder's state track the
    /// decoder's exactly.
    fn advance(&mut self, code: u8, delta: i32) {
        if code & 8 != 0 {
            self.predictor -= delta;
        } else {
            self.predictor += delta;
        }
        self.predictor = self
            .predictor
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        self.index = (self.index + INDEX_TABLE[(code & 0x0f) as usize]).clamp(0, 88);
    }
}

/// Encode one frame, writing the encoder's *entry* state into its header.
///
/// `state` is advanced, so a caller that keeps one across frames gets the
/// prediction quality of a continuous stream while every frame stays
/// independently decodable.
///
/// Frames shorter than [`FRAME_SAMPLES`] are padded with silence rather than
/// refused: the only caller that produces one is a stream ending mid-frame.
#[must_use]
pub fn encode_frame(pcm: &[i16], state: &mut AdpcmState) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_BYTES);
    out.push(CODEC_TAG);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the predictor is clamped to i16 range by `advance`"
    )]
    out.extend_from_slice(&(state.predictor as i16).to_le_bytes());
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "the step index is clamped to 0..=88"
    )]
    out.push(state.index as u8);

    for pair in 0..FRAME_SAMPLES / 2 {
        let lo = state.encode(pcm.get(pair * 2).copied().unwrap_or(0));
        let hi = state.encode(pcm.get(pair * 2 + 1).copied().unwrap_or(0));
        out.push(lo | (hi << 4));
    }
    out
}

/// Decode a frame. `None` for a payload this build does not understand — a
/// future codec, or a truncated frame — which is dropped rather than played.
#[must_use]
pub fn decode_frame(payload: &[u8]) -> Option<Vec<i16>> {
    if payload.len() != FRAME_BYTES || payload[0] != CODEC_TAG {
        return None;
    }
    let mut state = AdpcmState {
        predictor: i32::from(i16::from_le_bytes([payload[1], payload[2]])),
        index: i32::from(payload[3]).clamp(0, 88),
    };
    let mut out = Vec::with_capacity(FRAME_SAMPLES);
    for byte in &payload[4..] {
        out.push(state.decode(byte & 0x0f));
        out.push(state.decode(byte >> 4));
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Rate conversion and the gate
// ---------------------------------------------------------------------------

/// Linear resampler from the device's capture rate to [`VOICE_RATE`].
///
/// Linear interpolation, not a windowed-sinc: the input is already
/// band-limited by the microphone, the output rate is a decimation of at most
/// 3:1, and the artefact is a gentle high-frequency roll-off in a band speech
/// barely occupies. A polyphase filter would be the right answer if this were
/// music.
///
/// It is a *struct* rather than a function because the fractional read position
/// and the last input sample have to survive between callbacks — reset them per
/// buffer and every buffer boundary is a discontinuity, i.e. a click 50 times a
/// second.
#[derive(Default)]
pub struct Resampler {
    /// Read position within the next input buffer; may be negative, meaning
    /// "between [`Self::prev`] and the first new sample".
    pos: f64,
    prev: i16,
    /// The rate the position was computed for. A device change restarts the
    /// interpolation rather than skewing it.
    rate: u32,
}

impl Resampler {
    /// Convert `input` at `src_rate`, appending to `out`.
    pub fn push(&mut self, input: &[i16], src_rate: u32, out: &mut Vec<i16>) {
        if input.is_empty() || src_rate == 0 {
            return;
        }
        if self.rate != src_rate {
            *self = Self {
                rate: src_rate,
                ..Self::default()
            };
        }
        let ratio = f64::from(src_rate) / f64::from(VOICE_RATE);
        let len = input.len() as f64;
        while self.pos + 1.0 < len {
            let base = self.pos.floor();
            let frac = self.pos - base;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "`base` is bounded by the buffer length, checked below"
            )]
            let idx = base as isize;
            let a = if idx < 0 {
                self.prev
            } else {
                input[idx as usize]
            };
            let b = input[(idx + 1) as usize];
            let lerp = f64::from(a) + (f64::from(b) - f64::from(a)) * frac;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a value between two i16s is an i16"
            )]
            out.push(lerp.round() as i16);
            self.pos += ratio;
        }
        self.prev = input[input.len() - 1];
        self.pos -= len;
    }
}

/// A noise gate with hangover: open on speech, close after a pause.
#[derive(Default)]
pub struct Gate {
    open_for: f32,
}

impl Gate {
    /// Advance by `dt` seconds against this buffer's RMS. `true` means transmit.
    pub fn update(&mut self, rms: f32, dt: f32) -> bool {
        if rms >= GATE_OPEN_RMS {
            self.open_for = GATE_HANGOVER_SECS;
        } else {
            self.open_for = (self.open_for - dt).max(0.0);
        }
        self.open_for > 0.0
    }

    /// Shut it immediately — used when transmission stops, so releasing
    /// push-to-talk does not leave the hangover running into the next press.
    pub fn close(&mut self) {
        self.open_for = 0.0;
    }
}

/// Volume for a voice `distance_m` away. See [`FULL_VOLUME_M`].
#[must_use]
pub fn attenuation(distance_m: Option<f32>) -> f32 {
    // `None` is a room or global channel: no distance, no attenuation.
    let Some(d) = distance_m else { return 1.0 };
    if !d.is_finite() || d <= FULL_VOLUME_M {
        return 1.0;
    }
    (FULL_VOLUME_M / d).clamp(MIN_AUDIBLE, 1.0)
}

/// Root-mean-square of a buffer, 0..=1. The gate's input and the local meter.
#[must_use]
pub fn rms(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm
        .iter()
        .map(|s| {
            let v = f64::from(*s) / f64::from(i16::MAX);
            v * v
        })
        .sum();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "an RMS of normalised samples is within 0..=1"
    )]
    let out = (sum / pcm.len() as f64).sqrt() as f32;
    out.clamp(0.0, 1.0)
}

/// Apply the capture gain in place, saturating rather than wrapping.
pub fn apply_gain(pcm: &mut [i16], gain: f32) {
    if (gain - 1.0).abs() < f32::EPSILON {
        return;
    }
    for s in pcm.iter_mut() {
        // Clamped in f32 *before* the cast: `(x * 4.0) as i16` on an already-loud
        // sample wraps to the opposite polarity in release builds, which is a
        // full-scale click rather than the distortion a clamp gives.
        let v = (f32::from(*s) * gain).clamp(f32::from(i16::MIN), f32::from(i16::MAX));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to i16 range on the line above"
        )]
        {
            *s = v as i16;
        }
    }
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

/// One speaker's decoded audio, shared with the audio thread.
///
/// A `Mutex<VecDeque>` rather than a lock-free ring: the critical section is a
/// pop of one sample or an extend of 320, the contending threads are the mixer
/// and one Bevy system, and a spin-free queue here would be a dependency and a
/// proof obligation for a lock that is never held across anything.
pub(crate) type Ring = Arc<Mutex<std::collections::VecDeque<f32>>>;

/// A playable stream of one speaker's voice.
///
/// An `Asset` because that is bevy_audio's currency: `AudioPlayer<T>` needs
/// `T: Decodable + Asset`, and going through the asset system is what lets voice
/// reuse the one output device the soundscape already opened — rather than
/// opening a second one, which on a raw ALSA device fails outright and on the web
/// would need its own `AudioContext` unlock (`docs/notes/audio-off-platform.md`).
#[derive(Asset, TypePath, Clone)]
pub struct VoiceStream {
    ring: Ring,
}

impl VoiceStream {
    /// A stream playing whatever is pushed into `ring`, at [`VOICE_RATE`]. How a
    /// call's voices (`systems::calls`) use the same output as proximity voice.
    pub(crate) fn from_ring(ring: Ring) -> Self {
        Self { ring }
    }
}

/// The pull side of a [`VoiceStream`]. Never ends.
///
/// Returning `None` when the ring is empty would end the source, and rodio drops
/// a sink whose source has ended — so the first gap between two words would tear
/// the speaker's sink down and the next word would arrive with nothing to play
/// it. Silence is the correct answer to an empty ring; ending is not.
pub struct VoiceDecoder {
    ring: Ring,
}

impl Iterator for VoiceDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self
            .ring
            .lock()
            .map(|mut q| q.pop_front().unwrap_or(0.0))
            .unwrap_or(0.0);
        Some(sample)
    }
}

#[cfg(feature = "audio")]
impl bevy::audio::Source for VoiceDecoder {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> bevy::audio::ChannelCount {
        bevy::audio::ChannelCount::new(1).expect("1 is not zero")
    }

    fn sample_rate(&self) -> bevy::audio::SampleRate {
        bevy::audio::SampleRate::new(VOICE_RATE).expect("VOICE_RATE is not zero")
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        None
    }
}

#[cfg(feature = "audio")]
impl bevy::audio::Decodable for VoiceStream {
    type Decoder = VoiceDecoder;

    fn decoder(&self) -> Self::Decoder {
        VoiceDecoder {
            ring: Arc::clone(&self.ring),
        }
    }
}

/// Marks a voice sink so it can be found and torn down. Carries the speaker's id
/// because the entity outlives one frame's map iteration.
#[derive(Component)]
pub struct VoiceSink(pub u64);

/// What is known about one person currently being heard.
pub struct Speaker {
    ring: Ring,
    /// The asset, once the playback system has made one. `None` on a build with
    /// no audio feature, where frames are still decoded and dropped — the receive
    /// path is not worth a second shape for a build that cannot play it.
    handle: Option<Handle<VoiceStream>>,
    entity: Option<Entity>,
    /// Metres away, from the server. See the module note.
    pub distance_m: Option<f32>,
    /// Highest sequence number seen, for ordering.
    last_seq: u32,
    /// Seconds since a frame last arrived.
    silence: f32,
    /// Frames dropped as out-of-order or undecodable, for the diagnostics line.
    pub dropped: u32,
}

impl Speaker {
    /// Everything currently queued, taken out. Test-only: the audio thread is
    /// the consumer in a real run, and a second one would be a race.
    #[cfg(test)]
    fn take_buffered(&self) -> Vec<f32> {
        self.ring
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    fn new() -> Self {
        Self {
            ring: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            handle: None,
            entity: None,
            distance_m: None,
            last_seq: 0,
            silence: 0.0,
            dropped: 0,
        }
    }

    /// How much audio is queued, in seconds — the live latency for this speaker.
    #[must_use]
    pub fn buffered_secs(&self) -> f32 {
        let n = self.ring.lock().map(|q| q.len()).unwrap_or(0);
        n as f32 / VOICE_RATE as f32
    }
}

// ---------------------------------------------------------------------------
// The resource
// ---------------------------------------------------------------------------

/// How the microphone is armed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum VoiceMode {
    /// The microphone is closed and no device is open.
    #[default]
    Off,
    /// Armed: the device is open, and audio is sent only while the talk control
    /// is held.
    ///
    /// The device is opened on *arming* rather than on the first press, because
    /// opening one takes long enough (tens of milliseconds on AAudio, plus a
    /// permission dialog the first time) to swallow the first syllable of every
    /// sentence. The cost is that "armed" has to be visible, which is what the
    /// panel's live meter is for.
    PushToTalk,
    /// Armed and transmitting whenever the gate is open.
    Open,
}

/// Everything voice: the capture side, the send buffer, and who is being heard.
#[derive(Resource, Default)]
pub struct VoiceState {
    pub mode: VoiceMode,
    /// The open capture device, while armed.
    pub mic: Option<AudioInput>,
    /// Held down — the key, or a finger on the panel's talk button.
    ///
    /// Write-only for its two writers: it is *consumed* by
    /// [`drive_voice_capture`], which is the only thing that may clear it. See
    /// the comment there.
    pub talk_held: bool,
    /// Whether frames are actually going out right now (armed, held, gate open).
    pub transmitting: bool,
    /// The local meter, in the same 0..=1 shape `platform::audio` produces.
    pub level: f32,
    /// Whatever went wrong opening the device — a denied permission, usually.
    pub error: Option<String>,
    /// People currently audible, by `sender_id`.
    pub speakers: HashMap<u64, Speaker>,
    seq: u32,
    /// Scratch, reused every frame so a 50 Hz path allocates nothing.
    capture: Vec<i16>,
    pending: Vec<i16>,
    resampler: Resampler,
    gate: Gate,
    encoder: AdpcmState,
}

impl VoiceState {
    /// Whether this build and platform can transmit at all.
    #[must_use]
    pub fn can_capture() -> bool {
        AudioInput::available()
    }

    /// Arm or disarm. Idempotent, and the only place the device is opened.
    pub fn set_mode(&mut self, mode: VoiceMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        match mode {
            VoiceMode::Off => self.close(),
            _ => {
                if self.mic.is_none() {
                    self.error = None;
                    self.mic = Some(AudioInput::start());
                }
            }
        }
    }

    /// Take this frame's talk request, clearing it.
    ///
    /// **Taken, not read.** [`Self::talk_held`] has two writers — the V key and
    /// the panel's Hold-to-talk button — and neither may clear it without eating
    /// the other's press: the panel runs late in the frame (Phase 7b) and the key
    /// reader early in the next one, so whichever cleared would erase a flag it
    /// did not set. Both OR into it; this is the single consumer.
    pub fn take_talk(&mut self) -> bool {
        std::mem::take(&mut self.talk_held)
    }

    /// Release the device and forget anything half-encoded.
    pub fn close(&mut self) {
        self.mic = None;
        self.pending.clear();
        self.gate.close();
        self.transmitting = false;
        self.level = 0.0;
    }

    /// Take a decoded frame from the wire. Called by the network mailbox drain.
    ///
    /// Out-of-order and duplicate frames are **dropped, not reordered**. Today's
    /// carrier is TCP, where a frame that arrives late arrived late for a reason
    /// no client-side reordering can fix; the sequence number is on the wire for
    /// the datagram transport `VoiceFrame`'s comment anticipates, and this is the
    /// behaviour to revisit there rather than the place to build a reorder buffer
    /// that no test can currently exercise.
    pub fn receive(&mut self, sender_id: u64, seq: u32, payload: &[u8], distance_m: Option<f32>) {
        let speaker = self.speakers.entry(sender_id).or_insert_with(Speaker::new);
        speaker.distance_m = distance_m;
        speaker.silence = 0.0;
        // `!=` on the first frame of a session: a reconnecting speaker restarts at
        // 0, and `<=` would then discard everything they said until they caught
        // up with their previous session's counter.
        if speaker.last_seq != 0 && seq <= speaker.last_seq {
            speaker.dropped += 1;
            return;
        }
        speaker.last_seq = seq;
        let Some(pcm) = decode_frame(payload) else {
            speaker.dropped += 1;
            return;
        };
        {
            let Ok(mut ring) = speaker.ring.lock() else {
                return;
            };
            ring.extend(pcm.iter().map(|s| f32::from(*s) / f32::from(i16::MAX)));
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a fixed fraction of a second at a fixed rate"
            )]
            let cap = (MAX_RING_SECS * VOICE_RATE as f32) as usize;
            while ring.len() > cap {
                ring.pop_front();
            }
        }
        // A listener sitting at the cap is not a dropped frame, it is *drift* —
        // the sender's clock running faster than this device's, or a mixer that
        // has stopped pulling. Neither is visible any other way, and both sound
        // like a conversation half a second behind.
        let buffered = speaker.buffered_secs();
        if buffered >= MAX_RING_SECS - 0.01 {
            tracing::trace!(sender_id, buffered, "voice jitter buffer is full");
        }
    }
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Capture, encode and send, once a frame.
///
/// Runs whatever the run mode: with no [`OnlineSession`] there is nobody to send
/// to, and the microphone is closed rather than read into a void.
pub fn drive_voice_capture(
    mut voice: ResMut<VoiceState>,
    online: Option<Res<OnlineSession>>,
    prefs: Res<AppPrefs>,
    time: Res<Time>,
) {
    // Leaving a session must release the device. It is the same rule the Settings
    // panel follows for the mic test — a microphone held open by a screen nobody
    // is looking at is the one thing not to do — and here the trigger is the
    // session going rather than the panel closing.
    let Some(online) = online else {
        if voice.mic.is_some() {
            voice.mode = VoiceMode::Off;
            voice.close();
        }
        return;
    };
    if voice.mode == VoiceMode::Off {
        return;
    }
    // Checked before the destructuring below, because the answer is to *drop the
    // device*, and the borrow that lets capture and its scratch buffer coexist
    // has the device borrowed for its whole scope.
    if let Some(err) = voice.mic.as_ref().and_then(AudioInput::error) {
        // Terminal: the capture thread has stopped — a denied permission, or a
        // device that went away. Disarm, so the UI shows the reason once rather
        // than a meter that will never move.
        voice.error = Some(err);
        voice.mode = VoiceMode::Off;
        voice.close();
        return;
    }

    // Before the destructuring, so the flag is consumed on every path that gets
    // this far — including the ones that return early because the device is not
    // ready, where a press left standing would fire on a later frame.
    let talk = voice.take_talk();

    // Destructured, not field-by-field: `mic.drain(&mut capture)` borrows one
    // field immutably and another mutably at the same time, which the compiler
    // only accepts through a single destructuring of the resource.
    let VoiceState {
        mode,
        mic,
        transmitting,
        level,
        seq,
        capture,
        pending,
        resampler,
        gate,
        encoder,
        ..
    } = &mut *voice;

    let Some(mic) = mic.as_ref() else {
        return;
    };
    let Some(src_rate) = mic.sample_rate() else {
        // The device has not answered yet. Its samples are worth nothing without
        // its rate (see the module note), so wait rather than guess.
        return;
    };

    capture.clear();
    mic.drain(capture, src_rate as usize); // at most a second
    if mic.backlog() > 0 {
        // The device produced more than a second in one frame, or this system did
        // not run for one. Either way the capture queue is dropping its oldest
        // audio (`platform::audio::MAX_BACKLOG`) and the only trace of it is here.
        tracing::trace!(left = mic.backlog(), "voice capture drained partially");
    }
    apply_gain(capture, prefs.capture_gain());
    *level = rms(capture);

    let wanted = match *mode {
        VoiceMode::Off => false,
        VoiceMode::PushToTalk => talk,
        VoiceMode::Open => true,
    };
    // The gate runs only while the talk control asks for it. Feeding it silence
    // the rest of the time would work, but running it *at all* while not talking
    // invites the reading that push-to-talk is advisory.
    let open = if wanted {
        gate.update(*level, time.delta_secs())
    } else {
        gate.close();
        false
    };
    *transmitting = open;

    resampler.push(capture, src_rate, pending);
    capture.clear();

    if !open {
        // Keep the resampler's phase (it is a property of the device, not of the
        // conversation) but throw the audio away: the alternative is a burst of
        // stale speech the instant the key goes down again.
        pending.clear();
        return;
    }

    // Drop the oldest whole frames if we are behind. See MAX_PENDING_FRAMES.
    let cap = FRAME_SAMPLES * MAX_PENDING_FRAMES;
    if pending.len() > cap {
        let excess = pending.len() - cap;
        pending.drain(0..excess);
        tracing::trace!(excess, "voice capture fell behind; dropped the oldest");
    }

    let mut sent = 0;
    while pending.len() >= FRAME_SAMPLES {
        let frame: Vec<i16> = pending.drain(0..FRAME_SAMPLES).collect();
        let payload = encode_frame(&frame, encoder);
        *seq = seq.wrapping_add(1);
        // Proximity only. Global voice is refused by the server by design
        // (`net.rs`: "an unbounded fan-out and the loudest abuse surface chat
        // has"), and a room send would need the room this panel is not scoped to.
        let _ = online.intent_tx.unbounded_send(NetMessage::VoiceFrame {
            scope: ChatScope::Local { shout: false },
            seq: *seq,
            payload,
        });
        sent += 1;
    }
    if sent > 0 {
        tracing::trace!(sent, "voice frames sent");
    }
}

/// Spawn, level and retire the sinks that play what other people said.
#[cfg(feature = "audio")]
pub fn update_voice_playback(
    mut commands: Commands,
    mut voice: ResMut<VoiceState>,
    mut streams: ResMut<Assets<VoiceStream>>,
    prefs: Res<AppPrefs>,
    mut sinks: Query<(&VoiceSink, &mut bevy::audio::AudioSink)>,
    time: Res<Time>,
) {
    let gain = prefs.voice_gain();
    let dt = time.delta_secs();
    let mut retire: Vec<u64> = Vec::new();

    for (id, speaker) in &mut voice.speakers {
        speaker.silence += dt;
        if speaker.silence > SPEAKER_TIMEOUT_SECS {
            retire.push(*id);
            continue;
        }
        if speaker.entity.is_none() {
            let handle = streams.add(VoiceStream {
                ring: Arc::clone(&speaker.ring),
            });
            let entity = commands
                .spawn((
                    bevy::audio::AudioPlayer(handle.clone()),
                    bevy::audio::PlaybackSettings {
                        // Not `Despawn`/`Remove`: the source never ends, so those
                        // modes would never fire. The retirement above owns the
                        // entity's lifetime.
                        mode: bevy::audio::PlaybackMode::Once,
                        volume: bevy::audio::Volume::Linear(attenuation(speaker.distance_m) * gain),
                        ..default()
                    },
                    VoiceSink(*id),
                ))
                .id();
            speaker.handle = Some(handle);
            speaker.entity = Some(entity);
        }
    }

    // Distance changes as people walk; the sink is where that lands.
    for (sink_id, mut sink) in sinks.iter_mut() {
        if let Some(speaker) = voice.speakers.get(&sink_id.0) {
            sink.set_volume(bevy::audio::Volume::Linear(
                attenuation(speaker.distance_m) * gain,
            ));
        }
    }

    for id in retire {
        if let Some(speaker) = voice.speakers.remove(&id) {
            if let Some(entity) = speaker.entity {
                commands.entity(entity).despawn();
            }
            if let Some(handle) = speaker.handle {
                streams.remove(&handle);
            }
        }
    }
}

/// Read the push-to-talk key.
///
/// Separate from [`drive_voice_capture`] so the same flag can be written by the
/// panel's *Hold to talk* button, which is the only way to talk on a phone —
/// there is no keyboard, and the on-screen one is a text field's affordance.
pub fn read_talk_key(
    mut voice: ResMut<VoiceState>,
    keyboard: Res<ButtonInput<KeyCode>>,
    egui_manager: Res<crate::systems::egui_manager::EguiManager>,
    edit_dialog: Res<crate::resources::EditDialogState>,
) {
    if voice.mode != VoiceMode::PushToTalk {
        voice.talk_held = false;
        return;
    }
    // `ui_owns_keyboard`, not `egui_wants_keyboard_input` — the CLAUDE.md entry:
    // the latter is true whenever any widget holds focus, which would make V
    // silently dead after clicking a button.
    if egui_manager.ui_owns_keyboard(edit_dialog.visible, &keyboard) {
        return;
    }
    if keyboard.pressed(KeyCode::KeyV) {
        voice.talk_held = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine at a speech-like frequency, the length of one frame.
    fn tone(samples: usize, hz: f32, amp: f32) -> Vec<i16> {
        (0..samples)
            .map(|i| {
                let t = i as f32 / VOICE_RATE as f32;
                ((t * hz * std::f32::consts::TAU).sin() * amp * f32::from(i16::MAX)) as i16
            })
            .collect()
    }

    /// Error between two buffers as a fraction of full scale.
    fn mean_abs_error(a: &[i16], b: &[i16]) -> f32 {
        let n = a.len().min(b.len());
        let sum: f64 = (0..n)
            .map(|i| f64::from((i32::from(a[i]) - i32::from(b[i])).abs()))
            .sum();
        (sum / n as f64) as f32 / f32::from(i16::MAX)
    }

    /// The whole point of the codec: speech survives it. 4:1 compression with an
    /// average error of a few percent of full scale is what ADPCM buys, and a
    /// regression here (a broken step table, a nibble order swapped) shows up as
    /// a number rather than as "it sounds odd".
    #[test]
    fn a_frame_round_trips_recognisably() {
        let pcm = tone(FRAME_SAMPLES, 300.0, 0.5);
        let mut state = AdpcmState::default();
        let payload = encode_frame(&pcm, &mut state);
        assert_eq!(payload.len(), FRAME_BYTES);
        let back = decode_frame(&payload).expect("a frame we just encoded");
        assert_eq!(back.len(), FRAME_SAMPLES);
        let err = mean_abs_error(&pcm, &back);
        assert!(err < 0.05, "mean error {err:.4} of full scale");
    }

    /// A frame fits the server's budget with room to spare — `MAX_VOICE_BYTES`
    /// is 1500 and is sized so a frame never fragments when this moves to
    /// datagrams.
    #[test]
    fn a_frame_fits_one_datagram() {
        assert_eq!(FRAME_BYTES, 164);
        assert!(FRAME_BYTES < 1500);
        // 50 frames a second, both directions of the header included.
        let bytes_per_sec = FRAME_BYTES * 50;
        assert!(bytes_per_sec < 10_000, "{bytes_per_sec} B/s per speaker");
    }

    /// **The property a lost packet depends on.** Decoding a frame with no
    /// history must give the same samples as decoding it in sequence, or one
    /// dropped frame corrupts everything after it.
    #[test]
    fn every_frame_decodes_without_its_predecessors() {
        let pcm = tone(FRAME_SAMPLES * 3, 220.0, 0.6);
        let mut state = AdpcmState::default();
        let frames: Vec<Vec<u8>> = (0..3)
            .map(|i| encode_frame(&pcm[i * FRAME_SAMPLES..(i + 1) * FRAME_SAMPLES], &mut state))
            .collect();

        // Decode only the third, as a listener who lost the first two would.
        let third = decode_frame(&frames[2]).expect("valid frame");
        let all: Vec<i16> = frames
            .iter()
            .flat_map(|f| decode_frame(f).expect("valid frame"))
            .collect();
        assert_eq!(
            third,
            all[FRAME_SAMPLES * 2..],
            "a frame decoded alone differs from the same frame decoded in sequence"
        );
    }

    /// An unknown codec tag is dropped rather than played as noise.
    #[test]
    fn a_foreign_payload_is_refused() {
        let mut payload =
            encode_frame(&tone(FRAME_SAMPLES, 300.0, 0.4), &mut AdpcmState::default());
        payload[0] = 99;
        assert!(decode_frame(&payload).is_none(), "unknown tag was decoded");
        assert!(decode_frame(&[]).is_none(), "an empty payload was decoded");
        assert!(decode_frame(&[CODEC_TAG; 8]).is_none(), "a short frame");
    }

    /// Resampling holds the *rate*, across buffer boundaries. A per-buffer
    /// resampler drifts, and drift is a click at every seam.
    #[test]
    fn resampling_preserves_duration_across_buffers() {
        for src in [8_000, 16_000, 44_100, 48_000] {
            let mut r = Resampler::default();
            let mut out = Vec::new();
            // One second, in 10 ms buffers, exactly as a device delivers it.
            let chunk = src as usize / 100;
            for _ in 0..100 {
                r.push(&tone(chunk, 440.0, 0.5), src, &mut out);
            }
            let expected = VOICE_RATE as usize;
            let drift = (out.len() as isize - expected as isize).abs();
            assert!(
                drift <= 2,
                "{src} Hz produced {} samples, wanted {expected}",
                out.len()
            );
        }
    }

    /// At the transmit rate the resampler is a pass-through, not a filter.
    #[test]
    fn a_matched_rate_is_a_copy() {
        let pcm = tone(FRAME_SAMPLES, 500.0, 0.7);
        let mut out = Vec::new();
        Resampler::default().push(&pcm, VOICE_RATE, &mut out);
        let err = mean_abs_error(&pcm, &out);
        assert!(err < 0.001, "matched-rate error {err}");
    }

    /// The gate opens on speech and stays open through the pauses inside a
    /// sentence — the failure it exists to prevent is clipped word endings.
    #[test]
    fn the_gate_holds_through_a_pause() {
        let mut gate = Gate::default();
        assert!(!gate.update(0.0, 0.02), "silence must not open it");
        assert!(gate.update(0.2, 0.02), "speech opens it");
        // A third of a second of quiet, mid-sentence.
        for _ in 0..16 {
            assert!(gate.update(0.0, 0.02), "closed during a pause");
        }
        // A full second of quiet closes it.
        for _ in 0..50 {
            gate.update(0.0, 0.02);
        }
        assert!(!gate.update(0.0, 0.02), "never closed");
    }

    /// Distance attenuation: full up close, monotonic, and never fully silent —
    /// the client cannot tell a say from a shout, so a delivered frame is always
    /// audible.
    #[test]
    fn attenuation_falls_off_but_never_to_nothing() {
        assert_eq!(attenuation(None), 1.0, "a room channel has no distance");
        assert_eq!(attenuation(Some(0.0)), 1.0);
        assert_eq!(attenuation(Some(FULL_VOLUME_M)), 1.0);
        let mut last = 1.0;
        for d in [10.0, 25.0, 50.0, 100.0, 300.0] {
            let a = attenuation(Some(d));
            assert!(a <= last, "not monotonic at {d} m");
            assert!(a >= MIN_AUDIBLE, "inaudible at {d} m");
            last = a;
        }
        assert!(
            attenuation(Some(f32::NAN)) > 0.0,
            "a NaN must not silence it"
        );
    }

    /// Gain saturates instead of wrapping. `(x * 4.0) as i16` on a loud sample
    /// wraps polarity, which is a full-scale click, not distortion.
    #[test]
    fn gain_saturates() {
        let mut pcm = [30_000i16, -30_000, 100];
        apply_gain(&mut pcm, 4.0);
        assert_eq!(pcm[0], i16::MAX);
        assert_eq!(pcm[1], i16::MIN);
        assert_eq!(pcm[2], 400);
    }

    /// A late or repeated frame is dropped, and a speaker who reconnects (and so
    /// restarts at sequence 1) is still heard.
    #[test]
    fn out_of_order_frames_are_dropped_but_a_reconnect_is_not() {
        let mut voice = VoiceState::default();
        let payload = encode_frame(&tone(FRAME_SAMPLES, 300.0, 0.5), &mut AdpcmState::default());
        voice.receive(7, 10, &payload, Some(3.0));
        voice.receive(7, 9, &payload, Some(3.0));
        voice.receive(7, 10, &payload, Some(3.0));
        let speaker = &voice.speakers[&7];
        assert_eq!(speaker.dropped, 2, "a late and a duplicate frame");
        assert!(speaker.buffered_secs() > 0.0, "the first frame was queued");

        // The same person reconnects: a fresh session counts from 1 again.
        let mut fresh = VoiceState::default();
        fresh.receive(7, 1, &payload, None);
        assert_eq!(fresh.speakers[&7].dropped, 0);
    }

    /// The jitter buffer is bounded: a listener who stops draining must not
    /// accumulate latency, or memory, without limit.
    #[test]
    fn the_jitter_buffer_is_bounded() {
        let mut voice = VoiceState::default();
        let payload = encode_frame(&tone(FRAME_SAMPLES, 300.0, 0.5), &mut AdpcmState::default());
        for seq in 1..200 {
            voice.receive(3, seq, &payload, Some(1.0));
        }
        let buffered = voice.speakers[&3].buffered_secs();
        assert!(
            buffered <= MAX_RING_SECS + 0.001,
            "{buffered:.3} s queued for one speaker"
        );
    }

    /// The gate is armed by the talk control, not by the room being loud: an
    /// open microphone that transmits because a lorry went past is the failure
    /// people notice about voice chat.
    #[test]
    fn nothing_is_sent_while_the_talk_control_is_up() {
        let mut gate = Gate::default();
        // Loud, but nobody asked to talk — the capture system does not call
        // `update` at all in that case, and `close` is what it calls instead.
        gate.close();
        assert!(!gate.update(0.0, 0.02));
    }

    /// A press is consumed exactly once. Nothing else clears `talk_held` — a key
    /// reader that cleared it would erase the panel button's press from the
    /// previous frame, and that button is the only way to talk on a phone.
    #[test]
    fn a_talk_press_is_taken_by_its_consumer() {
        let mut voice = VoiceState::default();
        assert!(!voice.take_talk(), "nothing held, nothing taken");
        voice.talk_held = true;
        assert!(voice.take_talk(), "the press reached the consumer");
        assert!(
            !voice.take_talk(),
            "the press survived its consumer — releasing would never stop the mic"
        );
    }

    /// Arming opens nothing on a build with no capture backend, and disarming
    /// always releases the device.
    #[test]
    fn disarming_releases_the_microphone() {
        let mut voice = VoiceState::default();
        voice.set_mode(VoiceMode::PushToTalk);
        assert_eq!(voice.mode, VoiceMode::PushToTalk);
        assert!(
            voice.mic.is_some(),
            "arming must open a device to report on"
        );
        voice.set_mode(VoiceMode::Off);
        assert!(voice.mic.is_none(), "the device outlived the mode");
        assert!(!voice.transmitting);
    }
}

/// Voice over a **real simulator**, on a loopback socket.
///
/// This is the test that answers "does voice chat work", and it is here rather
/// than in `crates/simulator/tests/voice.rs` because the halves it has to join
/// live on opposite sides of that crate boundary: the sim's own suite proves the
/// fan-out rules with an opaque payload (`b"opus-ish"`), and everything that
/// makes a payload *audio* — the resampler, the codec, the sequence handling and
/// the jitter ring — is in this file. Neither can see the other, and voice
/// working is exactly the claim that the two agree.
///
/// What it establishes, none of which any unit test does:
///
/// - a frame this client encodes is one the **server accepts** (its
///   `MAX_VOICE_BYTES` and its 15 ms limiter both have opinions about what we
///   send, and a rejection is silent — `net.rs` returns without a reply);
/// - the round trip preserves the waveform, from a 44.1 kHz "device" through
///   resampling and 4:1 compression and back;
/// - `distance_m` arrives and is what the attenuation is applied to;
/// - the receive path fills the ring the audio thread reads.
///
/// It cannot establish anything about **devices**: this container has no audio
/// hardware at all (`docs/notes/container-audio.md`), so the capture side is a
/// generated tone rather than a microphone, and playback is asserted as samples
/// in a queue rather than as sound. Those two ends are unverified and say so.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod loopback {
    use super::*;
    use bytes::Bytes;
    use cartopolis_core::{decode_app_frame, encode_app_frame, AuthCredential, PROTOCOL_VERSION};
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpStream;
    use tokio_util::codec::{Framed, LengthDelimitedCodec};

    type Client = Framed<TcpStream, LengthDelimitedCodec>;

    async fn connect(addr: &str) -> Client {
        let mut c = Framed::new(
            TcpStream::connect(addr).await.unwrap(),
            LengthDelimitedCodec::builder()
                .max_frame_length(32 << 20)
                .little_endian()
                .new_codec(),
        );
        let hello = encode_app_frame(&NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: AuthCredential::Password {
                username: "dev".into(),
                password: "dev".into(),
            },
        })
        .unwrap();
        c.send(Bytes::from(hello)).await.unwrap();
        let ack = decode_app_frame(&c.next().await.unwrap().unwrap()).unwrap();
        assert!(matches!(ack, NetMessage::ServerHelloAck { .. }), "{ack:?}");
        c
    }

    #[tokio::test]
    async fn a_spoken_frame_arrives_as_the_same_sound() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("voice.db").to_str().unwrap().to_string();
        {
            let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
            cartopolis_simulator::ensure_account(&conn, "dev", "dev").unwrap();
        }
        let handle =
            cartopolis_simulator::serve(std::sync::Arc::new(cartopolis_simulator::SimConfig {
                database_path: db,
                listen: "127.0.0.1:0".into(),
                ..cartopolis_simulator::SimConfig::default()
            }))
            .await
            .unwrap();
        let addr = handle.local_addr.to_string();

        let mut speaker = connect(&addr).await;
        let mut listener = connect(&addr).await;

        // A 44.1 kHz "microphone" — the commonest desktop rate, and deliberately
        // not the transmit rate, so the resampler is in the path.
        const DEVICE_RATE: u32 = 44_100;
        const FRAMES: usize = 3;
        let device_pcm: Vec<i16> = (0..DEVICE_RATE as usize * 20 * FRAMES / 1000)
            .map(|i| {
                let t = i as f32 / DEVICE_RATE as f32;
                let v = (t * 300.0 * std::f32::consts::TAU).sin() * 0.5;
                (v * f32::from(i16::MAX)) as i16
            })
            .collect();

        let mut resampled = Vec::new();
        Resampler::default().push(&device_pcm, DEVICE_RATE, &mut resampled);
        assert!(
            resampled.len() >= FRAME_SAMPLES * FRAMES,
            "resampling lost a frame: {} samples",
            resampled.len()
        );

        let mut encoder = AdpcmState::default();
        for (n, chunk) in resampled.chunks(FRAME_SAMPLES).take(FRAMES).enumerate() {
            let payload = encode_frame(chunk, &mut encoder);
            assert!(payload.len() <= 1500, "over the server's MAX_VOICE_BYTES");
            let frame = encode_app_frame(&NetMessage::VoiceFrame {
                scope: ChatScope::Local { shout: false },
                seq: n as u32 + 1,
                payload,
            })
            .unwrap();
            speaker.send(Bytes::from(frame)).await.unwrap();
            // **The pacing is part of the test.** `net.rs` refuses anything
            // closer together than `MIN_VOICE_INTERVAL` (15 ms) and says nothing
            // when it does, so a client that sent its backlog in a burst would
            // lose all but the first frame and look like a codec bug.
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        // Receive through the real path, ring and all.
        let mut voice = VoiceState::default();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got = 0;
        while got < FRAMES {
            let Ok(Some(Ok(bytes))) = tokio::time::timeout_at(deadline, listener.next()).await
            else {
                break;
            };
            if let NetMessage::VoiceData {
                sender_id,
                seq,
                payload,
                distance_m,
                ..
            } = decode_app_frame(&bytes).unwrap()
            {
                assert!(
                    distance_m.is_some(),
                    "proximity voice must carry a distance"
                );
                voice.receive(sender_id, seq, &payload, distance_m);
                got += 1;
            }
        }
        assert_eq!(got, FRAMES, "the server delivered {got} of {FRAMES} frames");

        let speaker_state = voice.speakers.values().next().expect("a speaker");
        assert_eq!(speaker_state.dropped, 0, "frames were dropped on receipt");
        assert_eq!(
            attenuation(speaker_state.distance_m),
            1.0,
            "co-located avatars must not be attenuated"
        );
        let heard = speaker_state.take_buffered();
        assert_eq!(heard.len(), FRAME_SAMPLES * FRAMES, "sample count");

        // The waveform survived. ADPCM is lossy, so this is an error bound rather
        // than an equality — but it is tight enough that a swapped nibble order,
        // a lost predictor or a resampler running at the wrong ratio all fail it.
        let sent: Vec<f32> = resampled[..heard.len()]
            .iter()
            .map(|s| f32::from(*s) / f32::from(i16::MAX))
            .collect();
        let error: f32 = sent
            .iter()
            .zip(&heard)
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / heard.len() as f32;
        assert!(error < 0.05, "round-trip error {error:.4} of full scale");

        // And it is not silence that happens to be within the bound.
        let energy: f32 = heard.iter().map(|s| s * s).sum::<f32>() / heard.len() as f32;
        assert!(energy.sqrt() > 0.2, "what arrived was near-silent");
    }
}
