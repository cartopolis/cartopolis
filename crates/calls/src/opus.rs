//! The voice codec: Opus at 48 kHz mono in 20 ms frames, through `opus-pure`.
//!
//! Proximity voice uses IMA ADPCM because every Opus binding needed `cc`
//! (`docs/notes/voice-chat.md`). `opus-pure` has no C at all, so a call — which
//! runs over a lossy network for minutes at a time — gets the codec built for
//! exactly that: ~24 kbit/s instead of ADPCM's 64, in-band FEC that rebuilds a
//! lost frame from the next one, and concealment for the rest.

use opus_pure::{Application, OpusDecoder, OpusEncoder, MAX_PACKET_BYTES};

/// Opus's native rate, and WebRTC's.
pub const SAMPLE_RATE: u32 = 48_000;
/// 20 ms: the usual trade between latency and per-packet overhead.
pub const FRAME_SAMPLES: usize = 960;

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
    packet: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Result<Self, Error> {
        let mut inner = OpusEncoder::new(SAMPLE_RATE as i32, 1, Application::Voip).map_err(err)?;
        inner.bitrate_bps = BITRATE_BPS;
        inner.use_inband_fec = true;
        inner.packet_loss_perc = EXPECTED_LOSS_PERCENT;
        Ok(Self {
            inner,
            packet: vec![0; MAX_PACKET_BYTES],
        })
    }

    /// Encode exactly one frame of [`FRAME_SAMPLES`] mono samples in ±1.0.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, Error> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(Error(format!(
                "frame of {} samples, want {FRAME_SAMPLES}",
                pcm.len()
            )));
        }
        let n = self
            .inner
            .encode(pcm, FRAME_SAMPLES, &mut self.packet)
            .map_err(err)?;
        Ok(self.packet[..n].to_vec())
    }
}

pub struct Decoder {
    inner: OpusDecoder,
}

impl Decoder {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            inner: OpusDecoder::new(SAMPLE_RATE as i32, 1).map_err(err)?,
        })
    }

    /// Decode one packet into [`FRAME_SAMPLES`] samples.
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, Error> {
        let mut out = vec![0.0; FRAME_SAMPLES];
        self.inner
            .decode(packet, FRAME_SAMPLES, &mut out)
            .map_err(err)?;
        Ok(out)
    }

    /// A frame went missing and `next` is the one after it: rebuild the missing
    /// one from `next`'s FEC copy, or conceal it when there is no `next` yet.
    pub fn recover(&mut self, next: Option<&[u8]>) -> Result<Vec<f32>, Error> {
        let mut out = vec![0.0; FRAME_SAMPLES];
        match next {
            Some(p) => self.inner.decode_fec(p, FRAME_SAMPLES, &mut out),
            None => self.inner.decode(&[], FRAME_SAMPLES, &mut out),
        }
        .map_err(err)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frame: usize) -> Vec<f32> {
        (0..FRAME_SAMPLES)
            .map(|i| {
                let t = (frame * FRAME_SAMPLES + i) as f32 / SAMPLE_RATE as f32;
                0.5 * (t * 440.0 * std::f32::consts::TAU).sin()
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn a_tone_survives_at_the_call_bitrate_and_a_lost_frame_is_rebuilt() {
        let mut enc = Encoder::new().unwrap();
        let mut dec = Decoder::new().unwrap();
        let packets: Vec<Vec<u8>> = (0..50).map(|f| enc.encode(&tone(f)).unwrap()).collect();
        let biggest = packets.iter().map(Vec::len).max().unwrap();
        // 24 kbit/s is 60 bytes a frame; allow the encoder its variation.
        assert!(biggest < 160, "{biggest} bytes is not a 24 kbit/s stream");

        let mut last = Vec::new();
        for p in &packets[..40] {
            last = dec.decode(p).unwrap();
        }
        assert!(
            (rms(&last) - rms(&tone(39))).abs() < 0.03,
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
            (rms(&last) - rms(&tone(48))).abs() < 0.03,
            "recovered: {}",
            rms(&last)
        );
        // And with nothing to go on, concealment still produces a frame.
        assert_eq!(dec.recover(None).unwrap().len(), FRAME_SAMPLES);
        assert!(enc.encode(&[0.0; 10]).is_err());
    }
}
