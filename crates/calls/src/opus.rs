//! The voice codec: Opus, mono, 20 ms frames, through `opus-pure`.
//!
//! Proximity voice uses IMA ADPCM because every Opus binding needed `cc`
//! (`docs/notes/voice-chat.md`). `opus-pure` has no C at all, so a call — which
//! runs over a lossy network for minutes at a time — gets the codec built for
//! exactly that: ~24 kbit/s instead of ADPCM's 64, in-band FEC that rebuilds a
//! lost frame from the next one, and concealment for the rest.

use opus_pure::{Application, OpusDecoder, OpusEncoder, MAX_PACKET_BYTES};

/// The rates Opus codes at natively. The RTP clock is 48 kHz whatever the
/// encoder ran at, and a receiver may decode at any of these, so two ends need
/// not agree: the client captures at 16 kHz, the rate its proximity voice path
/// already resamples to, and wideband covers the whole of speech.
pub const RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Samples in one 20 ms frame at `rate` — the usual trade between latency and
/// per-packet overhead.
#[must_use]
pub const fn frame_samples(rate: u32) -> usize {
    (rate / 50) as usize
}

/// Speech at this rate is clear and leaves room for the FEC copy.
const BITRATE_BPS: i32 = 24_000;
/// What the encoder assumes when sizing FEC. Mobile networks lose a few percent.
const EXPECTED_LOSS_PERCENT: i32 = 5;

#[derive(Debug, thiserror::Error)]
#[error("opus: {0}")]
pub struct Error(String);

fn err(e: impl std::fmt::Display) -> Error {
    Error(e.to_string())
}

pub struct Encoder {
    inner: OpusEncoder,
    frame: usize,
    packet: Vec<u8>,
}

impl Encoder {
    /// An encoder taking mono PCM at `rate`, one of [`RATES`].
    pub fn new(rate: u32) -> Result<Self, Error> {
        let mut inner = OpusEncoder::new(rate as i32, 1, Application::Voip).map_err(err)?;
        inner.bitrate_bps = BITRATE_BPS;
        inner.use_inband_fec = true;
        inner.packet_loss_perc = EXPECTED_LOSS_PERCENT;
        Ok(Self {
            inner,
            frame: frame_samples(rate),
            packet: vec![0; MAX_PACKET_BYTES],
        })
    }

    /// Encode exactly one 20 ms frame of mono samples in ±1.0.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, Error> {
        if pcm.len() != self.frame {
            return Err(Error(format!(
                "frame of {} samples, want {}",
                pcm.len(),
                self.frame
            )));
        }
        let n = self
            .inner
            .encode(pcm, self.frame, &mut self.packet)
            .map_err(err)?;
        Ok(self.packet[..n].to_vec())
    }
}

pub struct Decoder {
    inner: OpusDecoder,
    frame: usize,
}

impl Decoder {
    /// A decoder producing mono PCM at `rate`, one of [`RATES`], whatever rate
    /// the sender encoded at.
    pub fn new(rate: u32) -> Result<Self, Error> {
        Ok(Self {
            inner: OpusDecoder::new(rate as i32, 1).map_err(err)?,
            frame: frame_samples(rate),
        })
    }

    /// Decode one 20 ms packet.
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, Error> {
        let mut out = vec![0.0; self.frame];
        let n = self
            .inner
            .decode(packet, self.frame, &mut out)
            .map_err(err)?;
        out.truncate(n);
        Ok(out)
    }

    /// A frame went missing and `next` is the one after it: rebuild the missing
    /// one from `next`'s FEC copy, or conceal it when there is no `next` yet.
    pub fn recover(&mut self, next: Option<&[u8]>) -> Result<Vec<f32>, Error> {
        let mut out = vec![0.0; self.frame];
        match next {
            Some(p) => self.inner.decode_fec(p, self.frame, &mut out),
            None => self.inner.decode(&[], self.frame, &mut out),
        }
        .map_err(err)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const FRAME: usize = frame_samples(RATE);

    fn tone(rate: u32, frame: usize) -> Vec<f32> {
        let n = frame_samples(rate);
        (0..n)
            .map(|i| {
                let t = (frame * n + i) as f32 / rate as f32;
                0.5 * (t * 440.0 * std::f32::consts::TAU).sin()
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn a_tone_survives_at_the_call_bitrate_and_a_lost_frame_is_rebuilt() {
        let mut enc = Encoder::new(RATE).unwrap();
        let mut dec = Decoder::new(RATE).unwrap();
        let packets: Vec<Vec<u8>> = (0..50)
            .map(|f| enc.encode(&tone(RATE, f)).unwrap())
            .collect();
        let biggest = packets.iter().map(Vec::len).max().unwrap();
        // 24 kbit/s is 60 bytes a frame; allow the encoder its variation.
        assert!(biggest < 160, "{biggest} bytes is not a 24 kbit/s stream");

        let mut last = Vec::new();
        for p in &packets[..40] {
            last = dec.decode(p).unwrap();
        }
        assert!(
            (rms(&last) - rms(&tone(RATE, 39))).abs() < 0.03,
            "steady: {}",
            rms(&last)
        );
        // Frame 40 is lost; 41 arrived and carries its FEC copy.
        let rebuilt = dec.recover(Some(&packets[41])).unwrap();
        // The FEC copy is coarser than the frame it stands in for (measured
        // 0.19 against the tone's 0.35), but it is the tone, not silence.
        assert!(
            rms(&rebuilt) > 0.1,
            "rebuilt frame is silence: {}",
            rms(&rebuilt)
        );
        // The decoder takes a few frames to climb back (0.08 on the first).
        for p in &packets[41..49] {
            last = dec.decode(p).unwrap();
        }
        assert!(
            (rms(&last) - rms(&tone(RATE, 48))).abs() < 0.03,
            "recovered: {}",
            rms(&last)
        );
        // And with nothing to go on, concealment still produces a frame.
        assert_eq!(dec.recover(None).unwrap().len(), FRAME);
        assert!(enc.encode(&[0.0; 10]).is_err());
    }

    #[test]
    fn a_16_khz_sender_is_heard_by_a_48_khz_receiver() {
        let mut enc = Encoder::new(16_000).unwrap();
        let mut dec = Decoder::new(RATE).unwrap();
        let mut last = Vec::new();
        for f in 0..30 {
            last = dec.decode(&enc.encode(&tone(16_000, f)).unwrap()).unwrap();
        }
        assert_eq!(last.len(), FRAME);
        assert!((rms(&last) - 0.354).abs() < 0.05, "{}", rms(&last));
    }
}
