//! A streaming resampler between the call's 16 kHz and the model's 24 kHz.
//!
//! Windowed-sinc interpolation, low-passed at 90 % of the lower rate's Nyquist
//! frequency. Linear interpolation would do for the way up, but the way down
//! takes the model's speech, which has energy above 8 kHz, and without the
//! low-pass that folds back into the band people hear as a hiss on every "s".

use std::f64::consts::PI;

/// Input samples either side of an output, at the lower rate. Eight is
/// plenty for speech, and costs nothing at these rates.
const HALF_TAPS: f64 = 8.0;

pub struct Resampler {
    /// Input rate over output rate: how far one output step moves in input.
    step: f64,
    /// The low-pass cut-off in cycles per input sample.
    cutoff: f64,
    /// Half the kernel's width, in input samples.
    half: f64,
    /// Input not yet behind the kernel.
    hist: Vec<f32>,
    /// Where the next output falls, in input samples from `hist[0]`.
    t: f64,
}

impl Resampler {
    #[must_use]
    pub fn new(from: u32, to: u32) -> Self {
        let (from, to) = (f64::from(from), f64::from(to));
        let lower = from.min(to);
        Self {
            step: from / to,
            cutoff: 0.5 * 0.9 * lower / from,
            half: HALF_TAPS * from / lower,
            hist: Vec::new(),
            // Start a kernel's half-width in, so the first output has history.
            t: 0.0,
        }
    }

    /// Resample `input`, appending to `out`. Output lags input by half a
    /// kernel (under a millisecond).
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.hist.extend_from_slice(input);
        while self.t + self.half < self.hist.len() as f64 {
            out.push(self.at(self.t));
            self.t += self.step;
        }
        // Drop what no later kernel reaches.
        let keep_from = (self.t - self.half).floor().max(0.0) as usize;
        if keep_from > 0 {
            self.hist.drain(..keep_from.min(self.hist.len()));
            self.t -= keep_from as f64;
        }
    }

    fn at(&self, t: f64) -> f32 {
        let lo = (t - self.half).ceil().max(0.0) as usize;
        let hi = ((t + self.half).floor() as usize).min(self.hist.len() - 1);
        let mut acc = 0.0f64;
        for (k, &x) in self.hist.iter().enumerate().take(hi + 1).skip(lo) {
            let d = t - k as f64;
            let arg = 2.0 * self.cutoff * d;
            let sinc = if arg.abs() < 1e-9 {
                1.0
            } else {
                (PI * arg).sin() / (PI * arg)
            };
            // Hann window over the kernel.
            let w = 0.5 + 0.5 * (PI * d / self.half).cos();
            acc += f64::from(x) * 2.0 * self.cutoff * sinc * w;
        }
        acc as f32
    }
}

/// `f32` samples, full scale 1, as little-endian PCM16 bytes.
#[must_use]
pub fn to_pcm16(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Little-endian PCM16 bytes as `f32` samples. A trailing odd byte is dropped.
#[must_use]
pub fn from_pcm16(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / f32::from(i16::MAX))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// Zero crossings a second: twice the frequency of a pure tone.
    fn crossings_per_s(x: &[f32], rate: u32) -> f32 {
        let n = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        n as f32 * rate as f32 / x.len() as f32
    }

    #[test]
    fn a_tone_keeps_its_pitch_and_level_both_ways() {
        let input = tone(16_000, 440.0, 16_000);
        let mut up = Resampler::new(16_000, 24_000);
        let mut at24 = Vec::new();
        // In 20 ms frames, as the call delivers them.
        for frame in input.chunks(320) {
            up.push(frame, &mut at24);
        }
        let n = at24.len() as f32;
        assert!((n - 24_000.0).abs() < 40.0, "{n} samples at 24 kHz");
        let mid = &at24[2_000..22_000];
        assert!((crossings_per_s(mid, 24_000) - 440.0).abs() < 3.0);
        assert!((rms(mid) - 0.3536).abs() < 0.01, "rms {}", rms(mid));

        let mut down = Resampler::new(24_000, 16_000);
        let mut back = Vec::new();
        for frame in at24.chunks(480) {
            down.push(frame, &mut back);
        }
        let mid = &back[2_000..14_000];
        assert!((crossings_per_s(mid, 16_000) - 440.0).abs() < 3.0);
        assert!((rms(mid) - 0.3536).abs() < 0.01, "rms {}", rms(mid));
    }

    /// What the low-pass is for: a 10 kHz tone in the model's speech has no
    /// place at 16 kHz, and without the filter it folds down to 6 kHz.
    #[test]
    fn energy_above_the_calls_band_is_removed_not_folded() {
        let input = tone(24_000, 10_000.0, 24_000);
        let mut down = Resampler::new(24_000, 16_000);
        let mut out = Vec::new();
        down.push(&input, &mut out);
        let mid = &out[1_000..15_000];
        assert!(rms(mid) < 0.02, "rms {} left of 0.35", rms(mid));
    }

    #[test]
    fn pcm16_round_trips() {
        let x = [0.0, 0.5, -0.5, 1.0, -1.0, 2.0];
        let back = from_pcm16(&to_pcm16(&x));
        for (a, b) in x.iter().zip(&back) {
            assert!((a.clamp(-1.0, 1.0) - b).abs() < 1e-4);
        }
    }
}
