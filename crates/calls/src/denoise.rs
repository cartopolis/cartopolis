//! Noise suppression for the app's microphone: RNNoise's recurrent network
//! (`nnnoiseless`, a pure-Rust port), which takes out fans, traffic and
//! keyboards and leaves the voice. The browser uses the platform's own
//! (`getUserMedia`'s `noiseSuppression`), so this is the app's alone.
//!
//! The network works on 10 ms frames at 48 kHz, so it runs on the microphone's
//! own samples before they are resampled for the voice codec, and only when
//! the device captures at 48 kHz, which is what most microphones run at;
//! [`Denoiser::process`] passes anything else through untouched. It delays the
//! sound by one frame, 10 ms.

use nnnoiseless::DenoiseState;

/// The rate the network was trained at.
pub const RATE: u32 = 48_000;
/// Its frame, 10 ms.
pub const FRAME: usize = DenoiseState::FRAME_SIZE;

/// One microphone's noise suppression, with the part-frame it is waiting on.
pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    pending: Vec<f32>,
    out: Vec<f32>,
    /// The first frame out is the network warming up, and clicks: dropped.
    warm: bool,
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl Denoiser {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            pending: Vec::with_capacity(FRAME * 2),
            out: vec![0.0; FRAME],
            warm: false,
        }
    }

    /// Denoise `pcm`, captured at `rate`, in place. At any rate but
    /// [`RATE`] it is left as it is. Whole frames come out as they are done,
    /// so over a stream as many samples leave as came in, up to 10 ms later.
    pub fn process(&mut self, pcm: &mut Vec<i16>, rate: u32) {
        if rate != RATE {
            return;
        }
        // The network takes samples at i16 scale, as floats.
        self.pending.extend(pcm.iter().map(|&s| f32::from(s)));
        pcm.clear();
        let mut done = 0;
        while self.pending.len() - done >= FRAME {
            let input = &self.pending[done..done + FRAME];
            self.state.process_frame(&mut self.out, input);
            done += FRAME;
            if !self.warm {
                self.warm = true;
                pcm.extend(std::iter::repeat_n(0, FRAME));
                continue;
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "clamped to the i16 range first"
            )]
            pcm.extend(
                self.out
                    .iter()
                    .map(|&s| s.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16),
            );
        }
        // A part-frame waits for the next call, so the stream stays
        // continuous: a call can return a little less or more than went in.
        self.pending.drain(..done);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steady white noise comes down once the network has heard a second of
    /// it, and the length of the stream is kept frame for frame.
    #[test]
    fn noise_comes_down_and_nothing_is_lost() {
        for level in [500, 2_000, 8_000] {
            let mut d = Denoiser::new();
            let mut seed = 0x1234_5678_u32;
            let mut noise = || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed % (2 * level)) as i32 - level as i32
            };
            let mut energy_in = 0f64;
            let mut energy_out = 0f64;
            let mut total_in = 0;
            let mut total_out = 0;
            // Three seconds in 20 ms buffers, as the app captures; the last
            // two are measured.
            for i in 0..150 {
                let mut pcm: Vec<i16> = (0..960).map(|_| noise() as i16).collect();
                total_in += pcm.len();
                let before = pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>();
                d.process(&mut pcm, RATE);
                total_out += pcm.len();
                if i >= 50 {
                    energy_in += before;
                    energy_out += pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>();
                }
            }
            assert_eq!(
                total_in, total_out,
                "every 20 ms buffer is two whole frames"
            );
            let left = energy_out / energy_in;
            // Measured 2026-10-17: 0.78, 0.27 and 0.69 of the energy left at
            // these three levels. The network was trained on recorded noise,
            // not flat white noise, so only the ordinary background level
            // (about -24 dBFS) is held to a real cut; none may come out louder.
            assert!(left < 1.0, "level {level}: louder out ({left:.2})");
            if level == 2_000 {
                assert!(left < 0.5, "background noise: {left:.2} of it left");
            }
        }
    }

    #[test]
    fn another_rate_passes_through() {
        let mut d = Denoiser::new();
        let mut pcm = vec![1000i16; 441];
        d.process(&mut pcm, 44_100);
        assert_eq!(pcm, vec![1000i16; 441]);
    }
}
