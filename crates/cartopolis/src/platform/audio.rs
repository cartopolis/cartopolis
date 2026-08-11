//! Microphone capture (ADR-030 substitutable seam).
//!
//! One API — [`AudioInput`] — over two entirely different mechanisms: `cpal` on
//! the desktop, `getUserMedia` + WebAudio in the browser. Feature code (the
//! Audio settings tab, and voice) programs against this and never branches on
//! platform.
//!
//! **Playback is not here.** The client already has an audio output path —
//! `systems::player::soundscape` synthesises clips into `Assets<AudioSource>`
//! and plays them through `bevy_audio` — so the sound test reuses that rather
//! than opening a second output device. Capture is the part Bevy has no answer
//! for: `bevy_audio` wraps `rodio`, which is output-only.
//!
//! # Why the desktop arm owns a thread
//!
//! `cpal::Stream` is not `Send` on every backend, and a Bevy `Resource` must be
//! `Send + Sync`. So the stream is built on, and owned by, a thread that parks
//! for its lifetime; the ECS side holds only an `Arc<Shared>` of atomics and one
//! small mutex. Dropping [`AudioInput`] signals that thread to let the stream go.
//!
//! # What "level" means
//!
//! [`AudioInput::level`] is a smoothed RMS in 0..=1, already the shape a meter
//! wants. It is computed in the capture callback rather than from drained frames
//! so that a meter works even when nothing is consuming audio — which is exactly
//! the mic-test case, where there is no voice session at all.
//!
//! # Verification status
//!
//! Neither arm has ever opened a real microphone: the agent container has no
//! audio device at all (`docs/notes/container-audio.md`). Both arms *compile*,
//! and the shared DSP is unit-tested.
//!
//! The web arm carries a second caveat that is easy to miss. The shipped web
//! build runs `--no-default-features` (`crates/cartopolis/index.html`), and `audio`
//! is a *default* feature — so the browser client is currently built **without**
//! this module's web arm, and [`AudioInput::available`] is `false` there. Voice
//! in the browser needs that feature turned on for wasm first; until then this
//! code is compiled only when someone asks for it explicitly.

// Without the `audio` feature there is no capture backend, so nothing calls the
// level maths, the downmix or the ring buffer — but they still compile and are
// still covered by the suite, exactly as `systems::player::soundscape` does it.
// The agent container and CI build `--no-default-features`, and this is what
// keeps the DSP under test on a host that cannot link ALSA at all.
#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Sample rate voice is normalised to. 48 kHz is Opus's native rate and what
/// WebAudio hands out, so choosing anything else buys a resampler on both arms.
pub const VOICE_SAMPLE_RATE: u32 = 48_000;

/// How many samples of backlog to keep before dropping the oldest.
///
/// Half a second at [`VOICE_SAMPLE_RATE`]. A capture callback that outruns its
/// consumer must drop *old* audio, not new: stale speech is worthless and an
/// unbounded queue turns a slow consumer into growing latency and then an OOM.
const MAX_BACKLOG: usize = VOICE_SAMPLE_RATE as usize / 2;

/// Smoothing applied to the level meter, per callback.
///
/// A bare per-buffer RMS flickers hard on speech, which reads as a broken meter.
/// Asymmetric on purpose: attack is near-instant so a clap registers, release is
/// slow so the bar falls back at a readable rate.
const LEVEL_ATTACK: f32 = 0.5;
const LEVEL_RELEASE: f32 = 0.08;

/// State shared between the capture callback and the ECS.
struct Shared {
    /// Smoothed RMS, 0..=1, as `f32` bits — read every frame by the meter.
    level: AtomicU32,
    /// Captured mono PCM awaiting a consumer.
    pcm: Mutex<std::collections::VecDeque<i16>>,
    /// Set when the owner is dropped; the capture thread exits on it.
    stop: AtomicBool,
    /// First fatal error, if the device failed to open or died.
    error: Mutex<Option<String>>,
    /// Device name, once known.
    device: Mutex<Option<String>>,
    /// Samples seen — lets a caller distinguish "silent" from "not running".
    samples: AtomicU32,
}

impl Shared {
    fn new() -> Self {
        Self {
            level: AtomicU32::new(0),
            pcm: Mutex::new(std::collections::VecDeque::new()),
            stop: AtomicBool::new(false),
            error: Mutex::new(None),
            device: Mutex::new(None),
            samples: AtomicU32::new(0),
        }
    }

    fn set_level(&self, v: f32) {
        self.level.store(v.to_bits(), Ordering::Relaxed);
    }

    fn fail(&self, msg: impl Into<String>) {
        *self.error.lock().unwrap() = Some(msg.into());
    }

    /// Fold one captured buffer in: update the meter, queue the samples.
    ///
    /// Shared by both arms, so the level maths and the drop policy cannot drift
    /// between desktop and web.
    fn push_buffer(&self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        let rms = rms_i16(samples);
        let prev = f32::from_bits(self.level.load(Ordering::Relaxed));
        self.set_level(smooth_level(prev, rms));
        self.samples
            .fetch_add(samples.len() as u32, Ordering::Relaxed);

        let mut q = self.pcm.lock().unwrap();
        q.extend(samples.iter().copied());
        // Drop from the front — see MAX_BACKLOG.
        while q.len() > MAX_BACKLOG {
            q.pop_front();
        }
    }
}

/// Root-mean-square of a mono i16 buffer, normalised to 0..=1.
#[must_use]
pub fn rms_i16(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|s| {
            let v = f64::from(*s) / f64::from(i16::MAX);
            v * v
        })
        .sum();
    ((sum / samples.len() as f64).sqrt() as f32).clamp(0.0, 1.0)
}

/// One step of the asymmetric meter smoothing.
#[must_use]
pub fn smooth_level(prev: f32, target: f32) -> f32 {
    let k = if target > prev {
        LEVEL_ATTACK
    } else {
        LEVEL_RELEASE
    };
    (prev + (target - prev) * k).clamp(0.0, 1.0)
}

/// Map a linear amplitude to a meter fill, on a dB scale.
///
/// A linear bar is useless for speech: normal talking sits around -20 dBFS, which
/// is 10% of a linear bar, so the meter looks broken until you shout. This maps
/// [`MIN_DB`]..0 dBFS onto 0..1.
pub const MIN_DB: f32 = -60.0;

#[must_use]
pub fn level_to_meter(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * level.log10();
    ((db - MIN_DB) / -MIN_DB).clamp(0.0, 1.0)
}

/// A running microphone capture.
///
/// Construct with [`AudioInput::start`]; poll [`AudioInput::level`] for a meter
/// and [`AudioInput::drain`] for PCM. Dropping it releases the device.
pub struct AudioInput {
    shared: Arc<Shared>,
    #[cfg(all(
        feature = "audio",
        not(target_arch = "wasm32"),
        not(target_os = "android")
    ))]
    _thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(all(feature = "audio", target_arch = "wasm32"))]
    _web: Option<web::WebCapture>,
}

impl AudioInput {
    /// Whether this build has a capture backend at all.
    ///
    /// Android is deliberately `false`: it needs its own backend (AAudio/oboe)
    /// and a runtime permission prompt, and neither is wired. Saying so is the
    /// `platform::location` contract — report unavailable rather than invent a
    /// silent, permanently-zero meter.
    /// Also `false` in a build without the `audio` feature — CI and the agent
    /// container build `--no-default-features`, and a meter that is structurally
    /// pinned at zero should say so rather than look like a dead microphone.
    #[must_use]
    pub const fn available() -> bool {
        cfg!(all(feature = "audio", not(target_os = "android")))
    }

    /// Open the default input device and begin capturing.
    ///
    /// Never fails outright — a device that cannot be opened yields a running
    /// `AudioInput` whose [`AudioInput::error`] is `Some`, so the UI has one
    /// shape to render instead of a `Result` at every call site.
    #[must_use]
    pub fn start() -> Self {
        let shared = Arc::new(Shared::new());

        #[cfg(all(
            feature = "audio",
            not(target_arch = "wasm32"),
            not(target_os = "android")
        ))]
        {
            let thread = native::spawn(Arc::clone(&shared));
            return Self {
                shared,
                _thread: thread,
            };
        }

        #[cfg(all(feature = "audio", target_arch = "wasm32"))]
        {
            let web = web::start(Arc::clone(&shared));
            return Self { shared, _web: web };
        }

        #[cfg(not(feature = "audio"))]
        {
            shared.fail("This build has no audio support (built without the `audio` feature)");
            Self { shared }
        }

        #[cfg(all(feature = "audio", target_os = "android"))]
        {
            shared.fail("Microphone capture is not implemented on Android yet");
            Self { shared }
        }
    }

    /// Smoothed input level, 0..=1. See [`level_to_meter`] for display.
    #[must_use]
    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.level.load(Ordering::Relaxed))
    }

    /// The device's name once it is open.
    #[must_use]
    pub fn device_name(&self) -> Option<String> {
        self.shared.device.lock().unwrap().clone()
    }

    /// The fatal error, if the device failed.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Whether any audio at all has arrived — "silent mic" vs "dead stream".
    #[must_use]
    pub fn is_receiving(&self) -> bool {
        self.shared.samples.load(Ordering::Relaxed) > 0
    }

    /// Move up to `max` queued samples out, appending to `out`.
    ///
    /// Returns how many were taken. Voice drains this every frame; the mic test
    /// does not drain at all, which is why the meter is maintained independently.
    #[expect(
        dead_code,
        reason = "the voice send path is the only consumer and lands next; the \
                  capture side is complete and tested without it"
    )]
    pub fn drain(&self, out: &mut Vec<i16>, max: usize) -> usize {
        let mut q = self.shared.pcm.lock().unwrap();
        let n = q.len().min(max);
        out.extend(q.drain(..n));
        n
    }

    /// Queued sample count, for diagnostics.
    #[expect(
        dead_code,
        reason = "diagnostic for the voice send path, which lands next"
    )]
    #[must_use]
    pub fn backlog(&self) -> usize {
        self.shared.pcm.lock().unwrap().len()
    }
}

impl Drop for AudioInput {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Desktop
// ---------------------------------------------------------------------------

#[cfg(all(
    feature = "audio",
    not(target_arch = "wasm32"),
    not(target_os = "android")
))]
mod native {
    use super::{Shared, VOICE_SAMPLE_RATE};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    /// Own the stream on a thread, because `cpal::Stream` is not `Send`.
    pub fn spawn(shared: Arc<Shared>) -> Option<std::thread::JoinHandle<()>> {
        std::thread::Builder::new()
            .name("cartopolis-mic".into())
            .spawn(move || run(&shared))
            .map_err(|e| tracing::warn!("could not spawn capture thread: {e}"))
            .ok()
    }

    fn run(shared: &Arc<Shared>) {
        let host = cpal::default_host();
        let Some(device) = host.default_input_device() else {
            shared.fail("No microphone found");
            return;
        };
        // `name()` is deprecated in cpal 0.17 in favour of the richer
        // `description()`; only the display name is wanted here.
        if let Ok(desc) = device.description() {
            *shared.device.lock().unwrap() = Some(desc.name().to_string());
        }
        let config = match device.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                shared.fail(format!("Microphone has no usable configuration: {e}"));
                return;
            }
        };

        let channels = config.channels() as usize;
        let rate = config.sample_rate();
        if rate != VOICE_SAMPLE_RATE {
            // Not fatal: the meter is rate-agnostic. Voice will need a resampler,
            // which is why the rate is logged rather than silently ignored.
            tracing::info!("microphone runs at {rate} Hz, not {VOICE_SAMPLE_RATE} Hz");
        }

        let err_shared = Arc::clone(shared);
        let on_error = move |e| err_shared.fail(format!("Microphone stream error: {e}"));

        // Downmix to mono in the callback: every consumer wants mono, and doing
        // it here keeps the interleave rule in exactly one place.
        let cb_shared = Arc::clone(shared);
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _: &_| {
                    let mono = downmix_f32(data, channels);
                    cb_shared.push_buffer(&mono);
                },
                on_error,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |data: &[i16], _: &_| {
                    let mono = downmix_i16(data, channels);
                    cb_shared.push_buffer(&mono);
                },
                on_error,
                None,
            ),
            other => {
                shared.fail(format!("Unsupported sample format {other:?}"));
                return;
            }
        };

        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                shared.fail(format!("Could not open the microphone: {e}"));
                return;
            }
        };
        if let Err(e) = stream.play() {
            shared.fail(format!("Could not start the microphone: {e}"));
            return;
        }

        // Hold the stream until the owner drops. Polling rather than parking on a
        // condvar keeps the shutdown path to one atomic and no extra primitive.
        while !shared.stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn downmix_f32(data: &[f32], channels: usize) -> Vec<i16> {
        super::downmix(data.len(), channels, |i| data[i])
    }

    fn downmix_i16(data: &[i16], channels: usize) -> Vec<i16> {
        super::downmix(data.len(), channels, |i| {
            f32::from(data[i]) / f32::from(i16::MAX)
        })
    }
}

/// Average `channels` interleaved samples into one, as i16.
///
/// Free function so both sample formats and the tests share it. Desktop-only:
/// the web arm requests a single channel from WebAudio and never interleaves,
/// so there is nothing to fold down there.
#[cfg(not(target_arch = "wasm32"))]
fn downmix(len: usize, channels: usize, at: impl Fn(usize) -> f32) -> Vec<i16> {
    let channels = channels.max(1);
    let frames = len / channels;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut acc = 0.0;
        for c in 0..channels {
            acc += at(f * channels + c);
        }
        let v = (acc / channels as f32).clamp(-1.0, 1.0);
        out.push((v * f32::from(i16::MAX)) as i16);
    }
    out
}

// ---------------------------------------------------------------------------
// Web
// ---------------------------------------------------------------------------

/// `getUserMedia` + WebAudio capture.
///
/// `echoCancellation` / `noiseSuppression` / `autoGainControl` are requested
/// because they are the entire reason to prefer the browser's capture path: AEC
/// is the hardest part of DIY voice, and here it is a constraint flag.
///
/// # Why the live graph is a thread-local rather than a field
///
/// `getUserMedia` resolves a promise, so the nodes only exist some time *after*
/// [`start`] returns and cannot be handed back to the caller. They are also
/// `!Send`, so they cannot live in the `Arc<Shared>` that crosses into the ECS.
/// The graph therefore lives in a thread-local (wasm is single-threaded) and
/// [`AudioInput`] holds a zero-sized [`WebCapture`] guard whose `Drop` tears it
/// down. Leaking it instead would leave the browser's recording indicator lit
/// after the user pressed Stop, which is precisely the thing not to do.
#[cfg(all(feature = "audio", target_arch = "wasm32"))]
mod web {
    use super::{Shared, VOICE_SAMPLE_RATE};
    use std::cell::RefCell;
    use std::sync::Arc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

    /// Buffer size for the capture callback: 2048 frames ≈ 43 ms at 48 kHz.
    /// Smaller means lower latency and more main-thread wakeups; an AudioWorklet
    /// is the upgrade when that trade stops being acceptable.
    const BUFFER_FRAMES: u32 = 2048;

    /// The live WebAudio graph. Dropping it disconnects and closes the context,
    /// which is what actually releases the microphone.
    struct Graph {
        ctx: web_sys::AudioContext,
        source: web_sys::MediaStreamAudioSourceNode,
        processor: web_sys::ScriptProcessorNode,
        stream: web_sys::MediaStream,
        _on_audio: Closure<dyn FnMut(web_sys::AudioProcessingEvent)>,
    }

    impl Drop for Graph {
        fn drop(&mut self) {
            self.processor.set_onaudioprocess(None);
            let _ = self.processor.disconnect();
            let _ = self.source.disconnect();
            // Stopping every track is what clears the browser's recording
            // indicator; closing the context alone does not.
            let tracks = self.stream.get_tracks();
            for i in 0..tracks.length() {
                if let Ok(track) = tracks.get(i).dyn_into::<web_sys::MediaStreamTrack>() {
                    track.stop();
                }
            }
            let _ = self.ctx.close();
        }
    }

    thread_local! {
        static LIVE: RefCell<Option<Graph>> = const { RefCell::new(None) };
    }

    /// Guard held by `AudioInput`; tears the graph down when the test stops.
    pub struct WebCapture;

    impl Drop for WebCapture {
        fn drop(&mut self) {
            LIVE.with(|l| l.borrow_mut().take());
        }
    }

    pub fn start(shared: Arc<Shared>) -> Option<WebCapture> {
        let window = web_sys::window()?;
        let media = window.navigator().media_devices().ok()?;

        let constraints = web_sys::MediaStreamConstraints::new();
        let audio = js_sys::Object::new();
        for (k, v) in [
            ("echoCancellation", true),
            ("noiseSuppression", true),
            ("autoGainControl", true),
        ] {
            let _ = js_sys::Reflect::set(&audio, &(*k).into(), &JsValue::from_bool(v));
        }
        constraints.set_audio(&audio.into());

        let promise = match media.get_user_media_with_constraints(&constraints) {
            Ok(p) => p,
            Err(e) => {
                shared.fail(describe_gum_error(&e));
                return Some(WebCapture);
            }
        };

        // The permission prompt is async and may be denied. Everything below runs
        // in the resolution handler, and a rejection becomes `Shared::error`, so
        // the settings tab can say "permission denied" rather than show a meter
        // that never moves.
        wasm_bindgen_futures::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(stream) => {
                    if let Err(e) = attach(&shared, stream.unchecked_into()) {
                        shared.fail(format!("Could not start capture: {e:?}"));
                    }
                }
                Err(e) => shared.fail(describe_gum_error(&e)),
            }
        });
        Some(WebCapture)
    }

    /// Turn a `getUserMedia` rejection into something a user can act on.
    fn describe_gum_error(e: &JsValue) -> String {
        let name = js_sys::Reflect::get(e, &"name".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        match name.as_str() {
            "NotAllowedError" | "SecurityError" => {
                "Microphone permission denied. Allow it in the browser's site settings.".into()
            }
            "NotFoundError" => "No microphone found.".into(),
            "NotReadableError" => "The microphone is in use by another application.".into(),
            _ => format!("Could not open the microphone ({name})"),
        }
    }

    fn attach(shared: &Arc<Shared>, stream: web_sys::MediaStream) -> Result<(), JsValue> {
        let opts = web_sys::AudioContextOptions::new();
        opts.set_sample_rate(VOICE_SAMPLE_RATE as f32);
        let ctx = web_sys::AudioContext::new_with_context_options(&opts)?;
        let source = ctx.create_media_stream_source(&stream)?;

        // ScriptProcessorNode is deprecated in favour of AudioWorklet, which needs
        // a separate JS module served beside the wasm. Kept deliberately: one
        // fewer file in the deploy, and the upgrade is local to this function.
        let processor = ctx
            .create_script_processor_with_buffer_size_and_number_of_input_channels_and_number_of_output_channels(
                BUFFER_FRAMES, 1, 1,
            )?;

        let cb_shared = Arc::clone(shared);
        let on_audio = Closure::<dyn FnMut(web_sys::AudioProcessingEvent)>::new(
            move |e: web_sys::AudioProcessingEvent| {
                let Ok(buf) = e.input_buffer() else { return };
                let Ok(data) = buf.get_channel_data(0) else {
                    return;
                };
                let pcm: Vec<i16> = data
                    .iter()
                    .map(|v| (v.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
                    .collect();
                cb_shared.push_buffer(&pcm);
            },
        );
        processor.set_onaudioprocess(Some(on_audio.as_ref().unchecked_ref()));

        source.connect_with_audio_node(&processor)?;
        // A ScriptProcessor only fires while connected to a destination, even
        // though its output is discarded — hence this otherwise pointless hop.
        processor.connect_with_audio_node(&ctx.destination())?;

        *shared.device.lock().unwrap() = Some("Browser microphone".into());
        LIVE.with(|l| {
            *l.borrow_mut() = Some(Graph {
                ctx,
                source,
                processor,
                stream,
                _on_audio: on_audio,
            });
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_of_silence_is_zero_and_full_scale_is_one() {
        assert_eq!(rms_i16(&[]), 0.0);
        assert_eq!(rms_i16(&[0; 128]), 0.0);
        // A full-scale square wave is RMS 1.0 by definition.
        let square: Vec<i16> = (0..128)
            .map(|i| if i % 2 == 0 { i16::MAX } else { -i16::MAX })
            .collect();
        assert!((rms_i16(&square) - 1.0).abs() < 0.001);
    }

    #[test]
    fn meter_attacks_fast_and_releases_slow() {
        // The asymmetry is the point: a transient must show, then fall readably.
        let up = smooth_level(0.0, 1.0);
        let down = smooth_level(1.0, 0.0);
        assert!(up > 0.4, "attack too slow to register a clap: {up}");
        assert!(down > 0.9, "release too fast to read: {down}");
    }

    #[test]
    fn meter_scale_is_db_not_linear() {
        // -20 dBFS is ordinary speech. On a linear bar it is 10%; it should read
        // as roughly two thirds here, or the meter looks broken until you shout.
        let speech = level_to_meter(0.1);
        assert!(
            (0.6..0.75).contains(&speech),
            "-20 dBFS should read ~2/3, got {speech}"
        );
        assert_eq!(level_to_meter(0.0), 0.0);
        assert!((level_to_meter(1.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn downmix_averages_channels_and_halves_the_length() {
        // Hard-panned stereo: L full scale, R silent -> mono at half scale.
        let interleaved = [1.0f32, 0.0, 1.0, 0.0];
        let mono = downmix(interleaved.len(), 2, |i| interleaved[i]);
        assert_eq!(mono.len(), 2);
        for s in mono {
            assert!((f32::from(s) / f32::from(i16::MAX) - 0.5).abs() < 0.01);
        }
    }

    #[test]
    fn backlog_drops_the_oldest_not_the_newest() {
        // Stale speech is worthless; an unbounded queue is an OOM. Newest wins.
        let shared = Shared::new();
        let chunk = vec![0i16; MAX_BACKLOG];
        shared.push_buffer(&chunk);
        let marker = vec![i16::MAX; 64];
        shared.push_buffer(&marker);

        let q = shared.pcm.lock().unwrap();
        assert_eq!(q.len(), MAX_BACKLOG, "backlog must stay bounded");
        let tail: Vec<i16> = q.iter().rev().take(64).copied().collect();
        assert!(
            tail.iter().all(|s| *s == i16::MAX),
            "the newest audio must survive"
        );
    }
}
